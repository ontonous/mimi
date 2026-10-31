#!/usr/bin/env python3
"""Collect a lossless default-route census for every Mimi source in a checkout.

This is deliberately separate from dispatch_stat.py.  It inventories every
repository ``*.mimi`` file (tracked and untracked), chooses an executable probe
for files with a top-level ``main`` and a shared-library probe otherwise, and
keeps complete command output alongside per-input route/dispatch observations.

Typical evidence collection (does not enforce a zero-fallback result):

    python3 scripts/full_route_census.py \\
      --mimi target/debug/mimi \\
      --output evidence/route-census-0.1.11-dev.json

Use ``--inventory-only`` to write the denominator and source/toolchain identity
without invoking Mimi or Cargo. A full run executes the two exact Actor Rust
authority tests, then uses a bounded worker pool and commits its journal and
progress in manifest order. Add
``--check`` to make the command a fail-closed gate: every denominator entry
must have an explicit current-source expectation whose result is proven. A
canonical compile expectation must build through the default Canonical route,
have a complete named per-function eligibility/emission roster, and have no
legacy fallback. An expected rejection must match its declared observed route
disposition, route reason, failure class, diagnostics, and absence of output.
The census parses the route-specific native emitter roster: canonical MIR builds
report every verified MIR owner and reconcile those names, counts, and graph
digest; resolved-native builds reconcile their named events against dispatch
and emitter totals.

Expectations are a typed, evidence-backed overlay. A mapped input binds its
Mimi source digest to a named test assertion and the digest/location of that
assertion's source file. Missing entries are reported as ``unmapped``; there is
no implicit success expectation. For example:

    {"schema_version": 4, "inputs": {
      "tests/fixtures/required.mimi": {
        "class": "required_compile",
        "reason": "fixture must compile through the canonical route",
        "source_sha256": "<64 lowercase hex characters>",
        "authority": {"test_id": "test_required_fixture_compiles",
          "assertion": {"path": "tests/route_contract.rs", "line": 12,
            "sha256": "<64 lowercase hex characters>"}}
      }
    }}

``expected_build_rejection`` entries also require a nonzero
``expected_exit_code``, a non-empty ``expected_diagnostic_codes`` list, a
non-empty ``required_output_substrings`` list, and explicit expected route
disposition, route-classification status, route reason, and failure class.
Those diagnostics must match the captured output exactly, and the build must
leave no output artifact. A valid
``interpreter_only`` entry skips the build and records ``run_status=not_run``.

An expected build rejection's named authority must be an active Rust ``#[test]``
that uses fully-qualified ``::std`` APIs to run the exact Cargo-provided Mimi
binary against the same repository-relative source with the default ``build``
command and asserts that command's failure, every expected diagnostic code and
output substring, and the complete route-rejection line containing the exact
route reason. Cargo authority records bind the absolute Cargo and rustc dispatch
paths, resolved executable hashes before and after each invocation, the frozen
toolchain/environment identity, and reject compiler, cfg, target, linker, runner,
Rust flag, or relevant Cargo config overrides. The census independently checks the observed route fields,
failure class, command exit, exact diagnostic-code set, required substrings,
and artifact absence.

Expectations never remove an input from the denominator. A valid
``interpreter_only`` mapping records an intentional no-build row with an
explicit reason; when a build runs, its stdout/stderr bytes and route remain
independent observations. An expected build rejection requires an exact set of
diagnostic codes, required output substrings, and no output artifact. The
manifest includes all tracked
``*.mimi`` paths (including tracked paths missing from disk) and all present
regular or symlink ``*.mimi`` files outside root-level ``devdocs/`` and
``evidence/`` archive roots and generated/vendor directory exclusions. Mimi
sources below archive roots remain in ``scope_exclusions`` with hashes.
Negative fixtures remain candidates and are classified as intentional
rejections only when an explicit expectation matches the observed diagnostics.
Symlinks and excluded directory names are listed as scope limitations. Build
stdout/stderr are retained inline and as exact-byte sidecars. Failure classes
are ``capability``, ``library_target``, ``environment``, ``unknown`` and
``none``; the collector does not relabel an unknown cause as a pass.

The JSON report is the top-level index. Full execution also writes
``<output>.artifacts/inventory.json`` (the frozen denominator),
``journal.jsonl`` (one durable record per inventory input), and ``logs/``
(exact stdout/stderr bytes for each build and the legacy-owner audit). The
scope excludes untracked files below generated/vendor directories and root-level
archive roots. Git-indexed Mimi paths remain accounted for below generated/vendor
directories or when missing, while archive sources stay in the exclusion ledger.
Symlinked directories are not traversed and Mimi file symlinks are recorded but
not followed. These boundaries are explicit and in-scope Mimi symlinks block
``--check``.
"""

from __future__ import annotations

import argparse
import base64
from collections import Counter
from concurrent.futures import Future, ThreadPoolExecutor
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Sequence

try:
    import tomllib
except ImportError:  # pragma: no cover - older Python fails closed on Cargo config
    tomllib = None

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_VERSION = "0.1.11-full-route-census-v6"
EXPECTATION_SCHEMA_VERSION = 4
EXPECTATION_CLASSES = {
    "required_compile",
    "expected_build_rejection",
    "interpreter_only",
    "known_gap",
    "unmapped",
    "environment_failure",
}
ARCHIVAL_SOURCE_PREFIXES = {
    "devdocs": "historical_or_staging_copy",
    "evidence": "preserved_evidence_source_copy",
}
ROUTE_CANDIDATE_CLASSIFICATIONS = {
    "canonical",
    "compatibility",
    "intentional_reject",
    "unmapped",
    "unknown",
}
ACTOR_FALLBACK_INPUTS = (
    "tests/real_world/actor_nested_func_capture.mimi",
    "tests/real_world/actor_nested_func_shadow.mimi",
)
ACTOR_AUTHORITY_TEST_IDS = {
    ACTOR_FALLBACK_INPUTS[0]: "canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback",
    ACTOR_FALLBACK_INPUTS[1]: "canonical_actor_shadow_original_uses_default_and_explicit_cli_without_fallback",
}
ACTOR_EXPECTED_STDOUT = {
    ACTOR_FALLBACK_INPUTS[0]: b"15\n",
    ACTOR_FALLBACK_INPUTS[1]: b"42\n101\n",
}
ACTOR_HELPER_TEST_PATH = "tests/real_world_cli.rs"
ACTOR_HELPER_NAME = "assert_original_actor_cli_routes"
# The Actor callsite tests exercise a substantial helper rather than issuing
# CLI assertions themselves. Pin its active implementation so changing it to
# a no-op cannot preserve authority merely by retaining the two callsites.
ACTOR_HELPER_SHA256 = "ff9ac0114e27da92f68d3b02a35c823e00a2a4ba91f4613d9dce0c01c90b24cc"
ACTIVE_LEGACY_OWNER_NAMES = (
    "CodegenLegacyRemainder",
    "FfiVerifierCompatibility",
)
RETIRED_LEGACY_OWNER_NAMES = (
    "FlowVerifierCompatibility",
    "DualVerifierCompatibility",
)
LEGACY_OWNER_NAMES = ACTIVE_LEGACY_OWNER_NAMES + RETIRED_LEGACY_OWNER_NAMES
RETIRED_FLOW_DUAL_PARITY_TESTS = (
    "flow_scalar_migration_keeps_mir_and_dual_verdicts",
    "flow_transition_invariant_and_field_projection_owner_parity",
    "canonical_mir_transition_preserves_contract_kinds",
)
EXCLUDED_DIRS = {
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "__pycache__",
    "node_modules",
    "target",
}
ENTRY_RE = re.compile(r"(?m)^[ \t]*(?:pub[ \t]+)?func[ \t]+main[ \t]*\(")
ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
ROUTE_RE = re.compile(
    r"canonical route disposition:\s*(canonical|legacy)\s*\((.*?)\)"
    r"(?:\s+mir_digest=([0-9a-f]{64}))?"
)
REJECT_RE = re.compile(r"default Canonical MIR route rejected:\s*(.*)")
EXPLICIT_REJECT_RE = re.compile(r"canonical MIR build rejected:\s*(.*)")
DIAGNOSTIC_RE = re.compile(
    r"(?im)^[ \t]*(?:error(?:\[[A-Z][A-Z0-9_-]*\])?(?:[ \t]*:|[ \t]+)|"
    r"E\d{4}:|type checking failed\b)"
)
SOURCE_DIAGNOSTIC_RE = re.compile(
    r"(?i)^[ \t]*error(?:\[[A-Z][A-Z0-9_-]*\])?(?:[ \t]+|[ \t]*:[ \t]+)"
    r"(?P<location>.*?\.mimi:\d+:\d+(?:-(?:\d+:)?\d+)?)(?:[ \t]+|$)"
)
DIAGNOSTIC_ERROR_CODE_RE = re.compile(
    r"(?i)^[ \t]*(?:"
    r"error[ \t]*\[[ \t]*(?P<bracketed>E\d{4}|MIR-[A-Z0-9_-]+-\d{3,})[ \t]*\](?=[ \t:]|$)"
    r"|error[ \t]*:[ \t]*(?P<error_code>E\d{4}|MIR-[A-Z0-9_-]+-\d{3,})(?=[ \t:]|$)"
    r"|(?P<bare>E\d{4})[ \t]*:"
    r")"
)
DIAGNOSTIC_CODE_FULL_RE = re.compile(r"(?:E\d{4}|MIR-[A-Z0-9_-]+-\d{3,})\Z", re.I)
TEST_DECLARATION_TEMPLATE = r"(?m)^[ \t]*(?:(?:pub|async|unsafe)[ \t]+)*(?:fn|def)[ \t]+{name}[ \t]*\("
ASSERTION_MARKER_RE = re.compile(
    r"\b(?:assert(?:_[A-Za-z0-9_]+)?|debug_assert(?:_[A-Za-z0-9_]+)?)(?:[ \t]*![ \t]*\(|[ \t]*\(|[ \t]+)"
    r"|\b(?:self\.)?assert[A-Z][A-Za-z0-9_]*[ \t]*\("
    r"|\b(?:ASSERT|EXPECT)_[A-Z0-9_]+[ \t]*\("
)
TOOLCHAIN_FAILURE_RE = re.compile(
    r"(?:failed to run linker|could not execute process|unable to execute|"
    r"no such file or directory.*(?:clang|llvm|gcc|cc))",
    re.I,
)
ENVIRONMENT_FAILURE_PATTERNS = (
    re.compile(
        r"(?:failed to run linker|linker.*not found|could not execute process|"
        r"unable to execute|command not found|No such file or directory.*"
        r"(?:clang|llvm|gcc|cc|z3)|z3.*not found|Permission denied|read-only file system|"
        r"No space left on device|cannot allocate memory|timed? out|spawn.*failed)",
        re.I,
    ),
)
LIBRARY_TARGET_FAILURE_PATTERNS = (
    re.compile(
        r"(?:(?:--shared|shared[- ](?:library|object)|library[- ]target).{0,120}"
        r"(?:unsupported|not supported|unavailable|rejected|failed|invalid|requires)|"
        r"(?:unsupported|not supported|unavailable|invalid).{0,120}"
        r"(?:--shared|shared[- ](?:library|object)|library[- ]target)|"
        r"(?:unknown|unexpected|unrecognized) (?:argument|option|flag).{0,80}--shared)",
        re.I,
    ),
)
CAPABILITY_FAILURE_PATTERNS = (
    re.compile(r"\bMIR-[A-Z0-9_-]+-\d{3,}\b", re.I),
    re.compile(
        r"(?:default Canonical MIR route rejected:|canonical MIR build rejected:|"
        r"canonical MIR .* rejected:|unsupported (?:MIR|Canonical|native|LLVM) (?:route|capability|operation))",
        re.I,
    ),
    re.compile(r"(?:error\[E072[23]\]|\bE072[23]\b).*", re.I),
    re.compile(r"\bNATIVE-GENERIC-PROTOCOL-ABI-001\b", re.I),
)
EXTERNAL_LINK_FAILURE_PATTERNS = (
    re.compile(
        r"\b(?:undefined symbol|undefined reference to)\s*:?\s*[`'\"]?"
        r"(?P<symbol>[\w.$@]+)",
        re.I,
    ),
)
SKIP_RE = re.compile(r"^info: resolved skip '(.+)': (.*)$")
EMIT_FALLBACK_RE = re.compile(r"^warning: resolved emitter fallback for '(.+)': (.*)$")
EMIT_VERIFY_FAIL_RE = re.compile(r"^warning: resolved emitter verification failed for '(.+)'$")
EMIT_SUCCESS_RE = re.compile(r"^info: resolved emitter emitted '(.+)'$")
EMIT_SUMMARY_RE = re.compile(
    r"^info: resolved emitter compiled (\d+)/(\d+) function\(s\), (\d+) fell back to legacy$"
)
MIR_EMIT_SUCCESS_PREFIX_RE = re.compile(
    r"info: canonical MIR native emitted owner="
)
MIR_EMIT_SUMMARY_PREFIX_RE = re.compile(
    r"info: canonical MIR native compiled "
)
MIR_OWNER_TOKEN_RE = re.compile(
    r"(?:function|transition):[A-Za-z0-9_.$:/<>-]+"
)
MIR_EMIT_SUCCESS_RE = re.compile(
    r"info: canonical MIR native emitted owner='(?P<owner>"
    + MIR_OWNER_TOKEN_RE.pattern
    + r")'"
)
MIR_EMIT_SUMMARY_RE = re.compile(
    r"info: canonical MIR native compiled (\d+)/(\d+) function\(s\), "
    r"(\d+) fell back; mir_digest=([0-9a-f]{64})$"
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _raw_capture(data: bytes, path: str | None = None) -> dict[str, Any]:
    capture: dict[str, Any] = {
        "sha256": sha256_bytes(data),
        "bytes": len(data),
        "base64": base64.b64encode(data).decode("ascii"),
    }
    if path is not None:
        capture["path"] = path
    return capture


def _captured_bytes(value: Any) -> bytes | None:
    if not isinstance(value, Mapping):
        return None
    encoded = value.get("base64")
    expected_hash = value.get("sha256")
    expected_bytes = value.get("bytes")
    if not isinstance(encoded, str) or not isinstance(expected_hash, str):
        return None
    try:
        raw = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error):
        return None
    if (
        sha256_bytes(raw) != expected_hash
        or not isinstance(expected_bytes, int)
        or isinstance(expected_bytes, bool)
        or len(raw) != expected_bytes
    ):
        return None
    return raw


def _strip_ansi(text: str) -> str:
    """Remove terminal control sequences for matching without changing captured logs."""
    return ANSI_ESCAPE_RE.sub("", text)


def _source_diagnostic_lines(text: str) -> list[str]:
    """Return raw compiler error lines that identify a Mimi source location."""
    return [
        line
        for line in text.splitlines()
        if SOURCE_DIAGNOSTIC_RE.search(_strip_ansi(line))
    ]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def relative_path(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix()


def source_category(path: Path, root: Path) -> str:
    parts = path.relative_to(root).parts
    if not parts:
        return "repository_root"
    if parts[0] == "demos":
        return "demo"
    if parts[0] == "examples":
        return "example"
    if parts[0] == "std":
        return "stdlib_source"
    if parts[0] == "projects":
        return "project_source"
    if parts[:2] == ("tests", "real_world"):
        return "real_world_test"
    if parts[:2] == ("tests", "fixtures"):
        return "test_fixture"
    if parts[0] == "tests":
        return "test_source"
    return "repository_source"


def source_record(path: Path, root: Path, *, tracked: bool = False) -> dict[str, Any]:
    source = path.read_bytes()
    text = source.decode("utf-8", errors="replace")
    has_main = ENTRY_RE.search(text) is not None
    try:
        mode = stat.S_IMODE(path.stat().st_mode)
    except OSError:
        mode = None
    return {
        "path": relative_path(path, root),
        "category": source_category(path, root),
        "target_kind": "executable" if has_main else "shared_library_probe",
        "target_kind_basis": "surface scan for a top-level func main; --shared is used when none is found",
        "main_detected_by_surface_scan": has_main,
        "present": True,
        "presence_kind": "regular_file",
        "buildable": True,
        "tracked_by_git": tracked,
        "bytes": len(source),
        "sha256": sha256_bytes(source),
        "mode": mode,
    }


def missing_source_record(path: Path, root: Path) -> dict[str, Any]:
    return {
        "path": relative_path(path, root),
        "category": source_category(path, root),
        "target_kind": "unknown",
        "target_kind_basis": "tracked source path is absent from the working tree",
        "main_detected_by_surface_scan": None,
        "present": False,
        "presence_kind": "missing_tracked_path",
        "buildable": False,
        "tracked_by_git": True,
        "bytes": None,
        "sha256": None,
        "mode": None,
    }


def _symlinked_parent(path: Path, root: Path) -> Path | None:
    """Return the first symlinked parent without resolving through it."""
    try:
        relative = path.relative_to(root)
    except ValueError:
        return None
    parent = root
    for part in relative.parts[:-1]:
        parent = parent / part
        try:
            metadata = parent.lstat()
        except OSError:
            return None
        if stat.S_ISLNK(metadata.st_mode):
            return parent
        if not stat.S_ISDIR(metadata.st_mode):
            return None
    return None


def blocked_source_record(
    path: Path, root: Path, *, tracked: bool, symlink_parent: Path
) -> dict[str, Any]:
    """Represent a tracked path unavailable behind a symlinked directory."""
    try:
        target_bytes = os.fsencode(os.readlink(symlink_parent))
        target_text: str | None = os.fsdecode(target_bytes)
        target_digest: str | None = sha256_bytes(target_bytes)
    except OSError:
        target_text = None
        target_digest = None
    return {
        "path": relative_path(path, root),
        "category": source_category(path, root),
        "target_kind": "unknown",
        "target_kind_basis": "tracked source is unavailable behind a symlinked parent directory",
        "main_detected_by_surface_scan": None,
        "present": False,
        "presence_kind": "blocked_by_symlinked_parent",
        "buildable": False,
        "tracked_by_git": tracked,
        "bytes": None,
        "sha256": None,
        "mode": None,
        "symlink_parent_path": relative_path(symlink_parent, root),
        "symlink_parent_target": target_text,
        "symlink_parent_target_sha256": target_digest,
    }


def symlink_source_record(path: Path, root: Path, *, tracked: bool) -> dict[str, Any]:
    try:
        target_bytes = os.fsencode(os.readlink(path))
        target_text: str | None = os.fsdecode(target_bytes)
        target_digest: str | None = sha256_bytes(target_bytes)
        target_length: int | None = len(target_bytes)
    except OSError:
        target_text = None
        target_digest = None
        target_length = None
    return {
        "path": relative_path(path, root),
        "category": source_category(path, root),
        "target_kind": "unknown",
        "target_kind_basis": "source symlinks are recorded but never followed or built",
        "main_detected_by_surface_scan": None,
        "present": True,
        "presence_kind": "symlink",
        "buildable": False,
        "tracked_by_git": tracked,
        "bytes": target_length,
        "sha256": target_digest,
        "symlink_target": target_text,
        "symlink_target_sha256": target_digest,
        "mode": None,
    }


def _is_archival_source_path(name: str) -> bool:
    return name.split("/", 1)[0] in ARCHIVAL_SOURCE_PREFIXES


def _scope_exclusion_record(path: Path, root: Path, *, tracked: bool) -> dict[str, Any]:
    """Retain a historical/evidence Mimi path without treating it as a route input."""
    name = relative_path(path, root)
    prefix = name.split("/", 1)[0]
    symlink_parent = _symlinked_parent(path, root)
    if symlink_parent is not None:
        record = blocked_source_record(
            path, root, tracked=tracked, symlink_parent=symlink_parent
        )
    elif path.is_symlink():
        record = symlink_source_record(path, root, tracked=tracked)
    elif path.is_file():
        record = source_record(path, root, tracked=tracked)
    else:
        record = missing_source_record(path, root)
    record.update({
        "route_candidate": False,
        "scope_classification": ARCHIVAL_SOURCE_PREFIXES[prefix],
        "scope_exclusion_reason": (
            "archived documentation, staging, or preserved evidence; retained in the source-scope "
            "ledger but outside the active route-candidate inventory"
        ),
    })
    return record


def tracked_mimi_paths(root: Path) -> tuple[list[str], dict[str, Any]]:
    """List Git-indexed Mimi paths without dropping deleted tracked inputs."""
    try:
        result = subprocess.run(
            ["git", "ls-files", "--cached", "-z", "--", "*.mimi"],
            cwd=root,
            capture_output=True,
            check=False,
        )
    except OSError as error:
        return [], {"status": "unavailable", "error": str(error)}
    if result.returncode != 0:
        return [], {
            "status": "unavailable",
            "exit_code": result.returncode,
            "stderr": _decode(result.stderr),
        }
    paths = sorted({Path(os.fsdecode(item)).as_posix() for item in result.stdout.split(b"\0") if item})
    return paths, {
        "status": "available",
        "path_count": len(paths),
        "paths_sha256": sha256_bytes("\0".join(paths).encode("utf-8")),
    }


def enumerate_source_scope(
    root: Path, tracked_paths: Sequence[str] | None = None
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    """Partition discoverable Mimi paths into active candidates and retained archives.

    ``devdocs`` and ``evidence`` often contain source snapshots and probes. They
    remain visible in ``scope_exclusions`` with a reason and hash, while all
    active source/test roots (including difficult and negative test fixtures)
    remain route candidates. This is a scope partition, not a build-result
    filter.
    """
    if tracked_paths is None:
        tracked_paths, _ = tracked_mimi_paths(root)
    tracked = set(tracked_paths)
    present_paths: set[str] = set()
    symlink_paths: set[str] = set()
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        dirnames[:] = sorted(
            name
            for name in dirnames
            if name not in EXCLUDED_DIRS and not (current / name).is_symlink()
        )
        for name in sorted(filenames):
            if not name.endswith(".mimi"):
                continue
            path = current / name
            if path.is_symlink():
                symlink_paths.add(relative_path(path, root))
                continue
            if not path.is_file():
                continue
            present_paths.add(relative_path(path, root))

    # A tracked source remains in the frozen denominator after deletion. Also
    # include tracked files located beneath a normally excluded directory: the
    # Git index, rather than a build-output directory heuristic, defines them
    # as intentional source inputs.
    all_paths = tracked | present_paths | symlink_paths
    sources: list[dict[str, Any]] = []
    scope_exclusions: list[dict[str, Any]] = []
    for name in sorted(all_paths):
        path = root / name
        symlink_parent = _symlinked_parent(path, root)
        if symlink_parent is not None:
            record = blocked_source_record(
                path, root, tracked=name in tracked, symlink_parent=symlink_parent
            )
            if _is_archival_source_path(name):
                prefix = name.split("/", 1)[0]
                record.update({
                    "route_candidate": False,
                    "scope_classification": ARCHIVAL_SOURCE_PREFIXES[prefix],
                    "scope_exclusion_reason": (
                        "archived source path is unavailable behind a symlinked parent directory"
                    ),
                })
                scope_exclusions.append(record)
            else:
                record["route_candidate"] = True
                record["scope_classification"] = "route_candidate"
                sources.append(record)
            continue
        if _is_archival_source_path(name):
            scope_exclusions.append(
                _scope_exclusion_record(path, root, tracked=name in tracked)
            )
            continue
        if path.is_symlink():
            record = symlink_source_record(path, root, tracked=name in tracked)
            record["route_candidate"] = True
            record["scope_classification"] = "route_candidate"
            sources.append(record)
            continue
        if path.is_file():
            record = source_record(path, root, tracked=name in tracked)
            record["route_candidate"] = True
            record["scope_classification"] = "route_candidate"
            sources.append(record)
        elif name in tracked:
            record = missing_source_record(path, root)
            record["route_candidate"] = True
            record["scope_classification"] = "route_candidate"
            sources.append(record)
    scoped_paths = [str(item["path"]) for item in sources + scope_exclusions]
    duplicate_paths = sorted(
        name for name, count in Counter(scoped_paths).items() if count > 1
    )
    scope = {
        "discoverable_mimi_path_count": len(all_paths),
        "candidate_count": len(sources),
        "excluded_count": len(scope_exclusions),
        "classification_counts": dict(sorted(Counter(
            str(item.get("scope_classification", "unknown"))
            for item in sources + scope_exclusions
        ).items())),
        "duplicate_paths": duplicate_paths,
        "unaccounted_paths": sorted(set(all_paths) - set(scoped_paths)),
        "exactly_once": (
            len(scoped_paths) == len(all_paths)
            and not duplicate_paths
            and not (set(all_paths) - set(scoped_paths))
        ),
        "archival_prefixes": dict(sorted(ARCHIVAL_SOURCE_PREFIXES.items())),
    }
    return sources, scope_exclusions, scope


def enumerate_sources(
    root: Path, tracked_paths: Sequence[str] | None = None
) -> list[dict[str, Any]]:
    """Return active route candidates; archival/evidence paths remain separately auditable."""
    sources, _scope_exclusions, _scope = enumerate_source_scope(root, tracked_paths)
    return sources


def source_symlink_exclusions(root: Path) -> list[dict[str, str]]:
    """List source symlinks excluded from the regular-file denominator."""
    excluded: list[dict[str, str]] = []
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        retained: list[str] = []
        for name in sorted(dirnames):
            path = current / name
            if path.is_symlink():
                target_bytes = os.fsencode(os.readlink(path))
                excluded.append({
                    "path": relative_path(path, root),
                    "kind": "directory_symlink_not_traversed",
                    "symlink_target": os.fsdecode(target_bytes),
                    "symlink_target_sha256": sha256_bytes(target_bytes),
                })
                continue
            if name in EXCLUDED_DIRS:
                continue
            retained.append(name)
        dirnames[:] = retained
        for name in sorted(filenames):
            path = current / name
            if name.endswith(".mimi") and path.is_symlink():
                target_bytes = os.fsencode(os.readlink(path))
                excluded.append({
                    "path": relative_path(path, root),
                    "kind": "mimi_file_symlink_not_followed",
                    "symlink_target": os.fsdecode(target_bytes),
                    "symlink_target_sha256": sha256_bytes(target_bytes),
                })
    return sorted(excluded, key=lambda item: (item["path"], item["kind"]))


def tracked_symlink_parent_blockers(
    root: Path, tracked_paths: Sequence[str]
) -> list[dict[str, Any]]:
    """Record tracked Mimi paths whose parent directory is a symlink."""
    blocked_paths: dict[str, set[str]] = {}
    targets: dict[str, tuple[str, str]] = {}
    for name in tracked_paths:
        path = root / name
        symlink_parent = _symlinked_parent(path, root)
        if symlink_parent is None:
            continue
        symlink_name = relative_path(symlink_parent, root)
        try:
            target_bytes = os.fsencode(os.readlink(symlink_parent))
            target_text = os.fsdecode(target_bytes)
            target_digest = sha256_bytes(target_bytes)
        except OSError:
            target_text = ""
            target_digest = ""
        blocked_paths.setdefault(symlink_name, set()).add(name)
        targets[symlink_name] = (target_text, target_digest)
    return [
        {
            "path": symlink_name,
            "kind": "tracked_mimi_path_parent_symlink_not_followed",
            "tracked_paths": sorted(names),
            "symlink_target": targets[symlink_name][0],
            "symlink_target_sha256": targets[symlink_name][1],
        }
        for symlink_name, names in sorted(blocked_paths.items())
    ]


def _compiler_snapshot_files(root: Path) -> list[Path]:
    """Hash Rust/build inputs plus the full Mimi input set, including untracked files."""
    files: set[Path] = set()
    explicit = [
        root / "Cargo.toml",
        root / "Cargo.lock",
        root / "build.rs",
        root / "scripts" / "full_route_census.py",
        root / "tests" / "scripts" / "test_full_route_census.py",
        root / "scripts" / "audit-mir-legacy-owners.sh",
        root / "rust-toolchain",
        root / "rust-toolchain.toml",
    ]
    explicit.extend(sorted((root / ".cargo").glob("**/*")) if (root / ".cargo").is_dir() else [])
    for path in explicit:
        if path.is_file() and not path.is_symlink():
            files.add(path)
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        dirnames[:] = sorted(
            name
            for name in dirnames
            if name not in EXCLUDED_DIRS and not (current / name).is_symlink()
        )
        for name in filenames:
            path = current / name
            if path.is_symlink():
                continue
            if path.suffix in {".rs", ".c", ".h", ".cc", ".cpp"}:
                files.add(path)
            elif name in {"build.rs", "build.sh"}:
                files.add(path)
    return sorted(files)


def source_snapshot(root: Path, sources: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    files: dict[str, dict[str, Any]] = {}
    for item in sources:
        files[item["path"]] = {
            "sha256": item.get("sha256"),
            "bytes": item.get("bytes"),
            "kind": {
                "regular_file": "mimi",
                "missing_tracked_path": "mimi_missing_tracked_input",
                "blocked_by_symlinked_parent": "mimi_blocked_by_symlinked_parent",
                "symlink": "mimi_symlink_not_followed",
            }.get(str(item.get("presence_kind")), "mimi_unknown_presence"),
            "symlink_target_sha256": item.get("symlink_target_sha256"),
        }
    for path in _compiler_snapshot_files(root):
        data = path.read_bytes()
        name = relative_path(path, root)
        files[name] = {"sha256": sha256_bytes(data), "bytes": len(data), "kind": "compiler_or_build_input"}
    digest = hashlib.sha256()
    for name, record in sorted(files.items()):
        digest.update(name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(str(record["sha256"] or "missing").encode("ascii"))
        digest.update(b"\0")
        digest.update(str(record["kind"]).encode("utf-8"))
        digest.update(b"\n")
    return {
        "snapshot_id": "sha256:" + digest.hexdigest(),
        "files": dict(sorted(files.items())),
        "file_count": len(files),
    }


def _command_result(
    command: Sequence[str], *, timeout: float = 8.0, cwd: Path | None = None
) -> dict[str, Any]:
    try:
        proc = subprocess.run(
            list(command),
            capture_output=True,
            timeout=timeout,
            check=False,
            cwd=cwd,
        )
        result = {
            "command": list(command),
            "exit_code": proc.returncode,
            "stdout": _decode(proc.stdout),
            "stderr": _decode(proc.stderr),
        }
        if cwd is not None:
            result["cwd"] = str(cwd.resolve())
        return result
    except subprocess.TimeoutExpired as error:
        result = {
            "command": list(command),
            "exit_code": None,
            "stdout": _decode(error.output),
            "stderr": _decode(error.stderr),
            "status": "timeout",
        }
        if cwd is not None:
            result["cwd"] = str(cwd.resolve())
        return result
    except OSError as error:
        result = {
            "command": list(command),
            "exit_code": None,
            "stdout": "",
            "stderr": str(error),
            "status": "spawn_error",
        }
        if cwd is not None:
            result["cwd"] = str(cwd.resolve())
        return result


def _decode(data: bytes | str | None) -> str:
    if data is None:
        return ""
    if isinstance(data, str):
        return data
    return data.decode("utf-8", errors="replace")


def _tool_identity(command_name: str) -> dict[str, Any]:
    executable = shutil.which(command_name)
    result: dict[str, Any] = {"requested": command_name, "path": executable}
    if executable:
        path = Path(executable)
        try:
            result["sha256"] = sha256_file(path)
            result["bytes"] = path.stat().st_size
        except OSError as error:
            result["hash_error"] = str(error)
    return result


def _native_executable_magic(path: Path) -> bool:
    """Reject script shims as Cargo/rustc identities when the host format is known."""
    try:
        with path.open("rb") as stream:
            prefix = stream.read(4)
    except OSError:
        return False
    system = platform.system()
    if system == "Linux":
        return prefix.startswith(b"\x7fELF")
    if system == "Darwin":
        return prefix in {
            b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",
            b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
            b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
        }
    if system == "Windows":
        return prefix[:2] == b"MZ"
    return False


def _executable_identity_at_path(
    command_name: str, path_value: str | Path | None
) -> dict[str, Any]:
    """Resolve a named executable path once to its canonical byte identity."""
    if path_value is None:
        return {
            "requested": command_name,
            "path": None,
            "sha256": None,
            "bytes": None,
            "native_executable": False,
        }
    try:
        invocation_path = Path(os.path.abspath(path_value))
        path = invocation_path.resolve(strict=True)
        metadata = path.stat()
        is_file = stat.S_ISREG(metadata.st_mode)
        executable = os.access(path, os.X_OK)
        native = is_file and executable and _native_executable_magic(path)
        digest = sha256_file(path) if is_file else None
        size = metadata.st_size if is_file else None
    except OSError as error:
        return {
            "requested": command_name,
            "path": str(Path(path_value).absolute()),
            "sha256": None,
            "bytes": None,
            "native_executable": False,
            "identity_error": str(error),
        }
    return {
        "requested": command_name,
        "path": str(invocation_path),
        "resolved_path": str(path),
        "sha256": digest,
        "bytes": size,
        "native_executable": native,
    }


def _resolved_executable_identity(command_name: str) -> dict[str, Any]:
    """Resolve an executable once to a canonical path and byte identity."""
    return _executable_identity_at_path(command_name, shutil.which(command_name))


def _rustup_input_files(root: Path) -> tuple[list[dict[str, Any]], list[str]]:
    """Snapshot every directory override and Rustup's default-toolchain settings."""
    root = root.resolve()
    candidates: list[Path] = []
    current = root
    while True:
        candidates.extend((current / "rust-toolchain", current / "rust-toolchain.toml"))
        if current.parent == current:
            break
        current = current.parent
    rustup_home = Path(os.environ.get("RUSTUP_HOME") or Path.home() / ".rustup")
    candidates.append(rustup_home / "settings.toml")

    records: list[dict[str, Any]] = []
    issues: list[str] = []
    for candidate in candidates:
        path = Path(os.path.abspath(candidate))
        record: dict[str, Any] = {"path": str(path)}
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            record["status"] = "missing"
        except OSError as error:
            record.update(status="unreadable", error=type(error).__name__)
            issues.append(f"rustup_input_unreadable:{path}")
        else:
            if stat.S_ISLNK(metadata.st_mode):
                try:
                    target = os.fsencode(os.readlink(path))
                    record["symlink_target_sha256"] = sha256_bytes(target)
                    record["symlink_target"] = os.fsdecode(target)
                except OSError:
                    record["symlink_target_sha256"] = None
                record["status"] = "symlink"
                issues.append(f"rustup_input_is_symlink:{path}")
            elif not stat.S_ISREG(metadata.st_mode):
                record["status"] = "not_regular_file"
                issues.append(f"rustup_input_not_regular:{path}")
            else:
                try:
                    raw = path.read_bytes()
                except OSError as error:
                    record.update(status="unreadable", error=type(error).__name__)
                    issues.append(f"rustup_input_unreadable:{path}")
                else:
                    record.update(
                        status="regular_file",
                        sha256=sha256_bytes(raw),
                        bytes=len(raw),
                    )
        records.append(record)
    return records, issues


def _rustup_toolchain_snapshot(
    root: Path,
    rustup_identity: Mapping[str, Any],
    *,
    probe: bool,
    expected: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Capture Rustup's cwd-specific selection and the actual selected binaries."""
    root = root.resolve()
    input_files, issues = _rustup_input_files(root)
    if not probe and expected is None:
        return {
            "cwd": str(root),
            "active_toolchain": None,
            "override_and_settings_files": input_files,
            "selected_cargo": _executable_identity_at_path("selected cargo", None),
            "selected_rustc": _executable_identity_at_path("selected rustc", None),
            "sysroot_path": None,
            "policy": {
                "valid": False,
                "issues": ["rustup_toolchain_not_probed_inventory_only"],
            },
        }
    active_toolchain: str | None = None
    selected_cargo_path: str | None = None
    selected_rustc_path: str | None = None

    if probe:
        rustup_path = rustup_identity.get("path")
        if not isinstance(rustup_path, str):
            issues.append("rustup_toolchain_probe_missing_rustup")
        else:
            active = _command_result(
                [rustup_path, "show", "active-toolchain"], cwd=root
            )
            cargo = _command_result([rustup_path, "which", "cargo"], cwd=root)
            rustc = _command_result([rustup_path, "which", "rustc"], cwd=root)

            active_lines = [line.strip() for line in active.get("stdout", "").splitlines() if line.strip()]
            if active.get("exit_code") == 0 and len(active_lines) == 1:
                active_toolchain = active_lines[0]
            else:
                issues.append("rustup_active_toolchain_probe_failed")

            def selected_path(result: Mapping[str, Any], label: str) -> str | None:
                lines = [line.strip() for line in str(result.get("stdout", "")).splitlines() if line.strip()]
                if result.get("exit_code") != 0 or len(lines) != 1 or not Path(lines[0]).is_absolute():
                    issues.append(f"rustup_selected_{label}_probe_failed")
                    return None
                return str(Path(lines[0]).absolute())

            selected_cargo_path = selected_path(cargo, "cargo")
            selected_rustc_path = selected_path(rustc, "rustc")
    elif isinstance(expected, Mapping):
        active_toolchain = expected.get("active_toolchain")
        expected_cargo = expected.get("selected_cargo")
        expected_rustc = expected.get("selected_rustc")
        selected_cargo_path = (
            expected_cargo.get("path") if isinstance(expected_cargo, Mapping) else None
        )
        selected_rustc_path = (
            expected_rustc.get("path") if isinstance(expected_rustc, Mapping) else None
        )
        if not isinstance(active_toolchain, str) or not active_toolchain:
            issues.append("rustup_active_toolchain_record_missing")
    else:
        issues.append("rustup_toolchain_not_probed_inventory_only")

    selected_cargo = _executable_identity_at_path("selected cargo", selected_cargo_path)
    selected_rustc = _executable_identity_at_path("selected rustc", selected_rustc_path)
    for label, identity in (("cargo", selected_cargo), ("rustc", selected_rustc)):
        if not identity.get("path") or not _is_sha256(identity.get("sha256")):
            issues.append(f"rustup_selected_{label}_identity_missing")
        if identity.get("native_executable") is not True:
            issues.append(f"rustup_selected_{label}_not_native_executable")

    cargo_resolved = selected_cargo.get("resolved_path")
    rustc_resolved = selected_rustc.get("resolved_path")
    cargo_sysroot = (
        str(Path(cargo_resolved).parent.parent)
        if isinstance(cargo_resolved, str)
        else None
    )
    rustc_sysroot = (
        str(Path(rustc_resolved).parent.parent)
        if isinstance(rustc_resolved, str)
        else None
    )
    if not cargo_sysroot or cargo_sysroot != rustc_sysroot:
        issues.append("rustup_selected_cargo_rustc_sysroots_mismatch")
    if not isinstance(active_toolchain, str) or not active_toolchain:
        issues.append("rustup_active_toolchain_missing")
    elif rustc_sysroot and Path(rustc_sysroot).name != active_toolchain.split(" (", 1)[0]:
        issues.append("rustup_active_toolchain_sysroot_name_mismatch")

    snapshot = {
        "cwd": str(root),
        "active_toolchain": active_toolchain,
        "override_and_settings_files": input_files,
        "selected_cargo": selected_cargo,
        "selected_rustc": selected_rustc,
        "sysroot_path": rustc_sysroot,
        "policy": {"valid": not issues, "issues": sorted(set(issues))},
    }
    if isinstance(expected, Mapping) and not probe:
        prior_policy = expected.get("policy")
        if not isinstance(prior_policy, Mapping) or prior_policy.get("valid") is not True:
            snapshot["policy"]["issues"] = sorted(
                set(snapshot["policy"]["issues"])
                | {"previous_rustup_toolchain_snapshot_invalid"}
            )
            snapshot["policy"]["valid"] = False
    return snapshot


def _executable_observation(path_value: Any) -> dict[str, Any]:
    """Read an executable's current canonical path and content hash."""
    if not isinstance(path_value, str) or not Path(path_value).is_absolute():
        return {"path": None, "sha256": None}
    try:
        invocation_path = Path(os.path.abspath(path_value))
        path = invocation_path.resolve(strict=True)
        metadata = path.stat()
        if not stat.S_ISREG(metadata.st_mode) or not os.access(path, os.X_OK):
            return {
                "path": str(invocation_path),
                "resolved_path": str(path),
                "sha256": None,
            }
        return {
            "path": str(invocation_path),
            "resolved_path": str(path),
            "sha256": sha256_file(path),
        }
    except OSError:
        return {"path": None, "resolved_path": None, "sha256": None}


_BLOCKED_CARGO_AUTHORITY_ENV = {
    "CARGO_HOME",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_BOOTSTRAP",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_TARGET",
    "CC",
    "CXX",
    "AR",
    "LD",
}


def _cargo_config_paths(root: Path) -> list[Path]:
    candidates: list[Path] = []
    current = root.resolve()
    while True:
        candidates.extend((current / ".cargo" / "config.toml", current / ".cargo" / "config"))
        if current.parent == current:
            break
        current = current.parent
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    candidates.extend((cargo_home / "config.toml", cargo_home / "config"))
    existing: dict[str, Path] = {}
    for candidate in candidates:
        try:
            if candidate.is_file():
                resolved = candidate.resolve(strict=True)
                existing[str(resolved)] = resolved
        except OSError:
            continue
    return [existing[key] for key in sorted(existing)]


def _cargo_config_authority(root: Path) -> dict[str, Any]:
    """Hash Cargo config inputs and reject compiler, cfg, linker or runner overrides."""
    files: list[dict[str, Any]] = []
    issues: list[str] = []
    for path in _cargo_config_paths(root):
        try:
            raw = path.read_bytes()
            text = raw.decode("utf-8")
            files.append({"path": str(path), "sha256": sha256_bytes(raw)})
            if tomllib is None:
                issues.append(f"cannot_parse_cargo_config:{path}")
                continue
            document = tomllib.loads(text)
        except (OSError, UnicodeDecodeError, ValueError) as error:
            issues.append(f"invalid_cargo_config:{path}:{type(error).__name__}")
            continue
        build = document.get("build", {})
        if isinstance(build, Mapping):
            for key in (
                "rustc", "rustc-wrapper", "rustc-workspace-wrapper", "target",
                "rustflags", "rustdocflags",
            ):
                if key in build:
                    issues.append(f"cargo_config_build_override:{path}:build.{key}")
        target = document.get("target")
        if isinstance(target, Mapping) and target:
            issues.append(f"cargo_config_target_override:{path}:target")
        configured_env = document.get("env", {})
        if isinstance(configured_env, Mapping):
            for name in configured_env:
                if not isinstance(name, str):
                    issues.append(f"invalid_cargo_config_env_key:{path}")
                elif (
                    name in _BLOCKED_CARGO_AUTHORITY_ENV
                    or name.startswith("CARGO_BUILD_")
                    or re.fullmatch(r"CARGO_TARGET_.+_(?:RUNNER|LINKER|RUSTFLAGS)", name)
                ):
                    issues.append(f"cargo_config_env_override:{path}:env.{name}")
    return {"files": files, "issues": sorted(set(issues)), "valid": not issues}


def cargo_test_toolchain_authority(
    root: Path,
    *,
    probe_active_toolchain: bool = True,
    expected_active_toolchain: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Freeze the Cargo/rustc identity and reject inherited build/runner overrides."""
    root = root.resolve()
    cargo = _resolved_executable_identity("cargo")
    rustc = _resolved_executable_identity("rustc")
    rustup = _resolved_executable_identity("rustup")
    environment = _environment_fingerprint()
    config = _cargo_config_authority(root)
    rustup_toolchain = _rustup_toolchain_snapshot(
        root,
        rustup,
        probe=probe_active_toolchain,
        expected=expected_active_toolchain,
    )
    issues: list[str] = list(config["issues"])
    issues.extend(rustup_toolchain["policy"]["issues"])
    for name in sorted(os.environ):
        value = os.environ[name]
        if not value:
            continue
        if (
            name in _BLOCKED_CARGO_AUTHORITY_ENV
            or name.startswith("CARGO_BUILD_")
            or name.startswith("CARGO_ALIAS_")
            or re.fullmatch(r"CARGO_TARGET_.+_(?:RUNNER|LINKER|RUSTFLAGS)", name)
        ):
            issues.append(f"inherited_toolchain_override:{name}")
    for label, identity in (("cargo", cargo), ("rustc", rustc), ("rustup", rustup)):
        if not identity.get("path") or not _is_sha256(identity.get("sha256")):
            issues.append(f"{label}_identity_missing")
        if identity.get("native_executable") is not True:
            issues.append(f"{label}_is_not_native_executable")
    # In this rustup-managed toolchain, cargo and rustc are argv[0] dispatch
    # symlinks to the same rustup ELF.  Pin that relation as well as each
    # invocation name: a native ELF named `cargo` earlier on PATH must not be
    # accepted just because its bytes are stable and executable.
    rustup_path = rustup.get("path")
    rustup_target = rustup.get("resolved_path")
    for label, identity in (("cargo", cargo), ("rustc", rustc)):
        if (
            not isinstance(rustup_path, str)
            or not isinstance(rustup_target, str)
            or identity.get("resolved_path") != rustup_target
            or not isinstance(identity.get("path"), str)
            or Path(identity["path"]).parent != Path(rustup_path).parent
        ):
            issues.append(f"{label}_is_not_the_selected_rustup_proxy")
    payload: dict[str, Any] = {
        "cargo": cargo,
        "rustc": rustc,
        "rustup": rustup,
        "rustup_toolchain": rustup_toolchain,
        "environment_sha256": environment["sha256"],
        "environment_variable_names": environment["variable_names"],
        "cargo_config": config,
        "policy": {"valid": not issues, "issues": sorted(set(issues))},
    }
    payload["identity_sha256"] = sha256_bytes(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
    return payload


def _cargo_toolchain_identity_valid(
    authority: Any,
    *,
    root: Path,
    require_current_environment: bool = True,
    current_authority: Mapping[str, Any] | None = None,
) -> bool:
    if not isinstance(authority, Mapping):
        return False
    identity = authority.get("identity_sha256")
    payload = {key: value for key, value in authority.items() if key != "identity_sha256"}
    policy = authority.get("policy")
    if (
        not isinstance(policy, Mapping)
        or policy.get("valid") is not True
        or not _is_sha256(identity)
        or sha256_bytes(json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")) != identity
    ):
        return False
    if not all(
        isinstance(authority.get(key), Mapping)
        and authority[key].get("native_executable") is True
        for key in ("cargo", "rustc", "rustup")
    ):
        return False
    for key in ("cargo", "rustc", "rustup"):
        expected = authority[key]
        observed = _executable_observation(expected.get("path"))
        if observed != {
            "path": expected.get("path"),
            "resolved_path": expected.get("resolved_path"),
            "sha256": expected.get("sha256"),
        }:
            return False
    if not all(
        authority[command].get("resolved_path") == authority["rustup"].get("resolved_path")
        and Path(str(authority[command].get("path"))).parent
        == Path(str(authority["rustup"].get("path"))).parent
        for command in ("cargo", "rustc")
    ):
        return False
    rustup_toolchain = authority.get("rustup_toolchain")
    if (
        not isinstance(rustup_toolchain, Mapping)
        or not isinstance(rustup_toolchain.get("policy"), Mapping)
        or rustup_toolchain["policy"].get("valid") is not True
    ):
        return False
    if require_current_environment:
        current = current_authority or cargo_test_toolchain_authority(
            root,
            probe_active_toolchain=False,
            expected_active_toolchain=rustup_toolchain,
        )
        if current.get("identity_sha256") != identity:
            return False
    return True


def _cargo_execution_evidence(
    authority: Mapping[str, Any], cargo_before: Mapping[str, Any],
    rustc_before: Mapping[str, Any], cargo_after: Mapping[str, Any],
    rustc_after: Mapping[str, Any], *, stable: bool,
    rustup_before: Mapping[str, Any] | None = None,
    rustup_after: Mapping[str, Any] | None = None,
    rustup_toolchain_before: Mapping[str, Any] | None = None,
    rustup_toolchain_after: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    if rustup_before is None:
        rustup_before = _executable_observation(authority.get("rustup", {}).get("path"))
    if rustup_after is None:
        rustup_after = rustup_before
    if rustup_toolchain_before is None:
        rustup_toolchain_before = authority.get("rustup_toolchain", {})
    if rustup_toolchain_after is None:
        rustup_toolchain_after = rustup_toolchain_before
    return {
        "toolchain_identity_sha256": authority.get("identity_sha256"),
        "cargo": {
            "expected_path": authority.get("cargo", {}).get("path"),
            "expected_sha256": authority.get("cargo", {}).get("sha256"),
            "before": dict(cargo_before),
            "after": dict(cargo_after),
        },
        "rustc": {
            "expected_path": authority.get("rustc", {}).get("path"),
            "expected_sha256": authority.get("rustc", {}).get("sha256"),
            "before": dict(rustc_before),
            "after": dict(rustc_after),
        },
        "rustup": {
            "expected_path": authority.get("rustup", {}).get("path"),
            "expected_sha256": authority.get("rustup", {}).get("sha256"),
            "before": dict(rustup_before),
            "after": dict(rustup_after),
        },
        "rustup_toolchain": {
            "expected": dict(authority.get("rustup_toolchain", {})),
            "before": dict(rustup_toolchain_before),
            "after": dict(rustup_toolchain_after),
        },
        "child_environment": {
            "RUSTC": authority.get("rustc", {}).get("path"),
            "RUSTC_WRAPPER": "unset",
            "RUSTC_WORKSPACE_WRAPPER": "unset",
            "target_runner": "unset",
            "rustflags": "unset",
        },
        "stable": stable,
    }


def _cargo_execution_evidence_valid(
    evidence: Any, authority: Any, *, root: Path
) -> bool:
    if not isinstance(evidence, Mapping) or not _cargo_toolchain_identity_valid(
        authority, root=root
    ):
        return False
    if evidence.get("toolchain_identity_sha256") != authority.get("identity_sha256"):
        return False
    for key in ("cargo", "rustc", "rustup"):
        row = evidence.get(key)
        identity = authority.get(key) if isinstance(authority, Mapping) else None
        if not isinstance(row, Mapping) or not isinstance(identity, Mapping):
            return False
        expected = {
            "path": identity.get("path"),
            "resolved_path": identity.get("resolved_path"),
            "sha256": identity.get("sha256"),
        }
        if (
            row.get("expected_path") != identity.get("path")
            or row.get("expected_sha256") != identity.get("sha256")
            or row.get("before") != expected
            or row.get("after") != expected
        ):
            return False
    rustup_toolchain_evidence = evidence.get("rustup_toolchain")
    expected_rustup_toolchain = authority.get("rustup_toolchain")
    if (
        not isinstance(rustup_toolchain_evidence, Mapping)
        or not isinstance(expected_rustup_toolchain, Mapping)
        or rustup_toolchain_evidence.get("expected") != expected_rustup_toolchain
        or rustup_toolchain_evidence.get("before") != expected_rustup_toolchain
        or rustup_toolchain_evidence.get("after") != expected_rustup_toolchain
    ):
        return False
    child_environment = evidence.get("child_environment")
    return bool(
        evidence.get("stable") is True
        and isinstance(child_environment, Mapping)
        and child_environment.get("RUSTC") == authority["rustc"]["path"]
        and child_environment.get("RUSTC_WRAPPER") == "unset"
        and child_environment.get("RUSTC_WORKSPACE_WRAPPER") == "unset"
        and child_environment.get("target_runner") == "unset"
        and child_environment.get("rustflags") == "unset"
    )


def _environment_fingerprint() -> dict[str, Any]:
    """Fingerprint the inherited environment without copying secret values."""
    digest = hashlib.sha256()
    names = sorted(os.environ)
    for name in names:
        key = os.fsencode(name)
        value = os.fsencode(os.environ[name])
        digest.update(len(key).to_bytes(8, "big"))
        digest.update(key)
        digest.update(len(value).to_bytes(8, "big"))
        digest.update(value)
    return {
        "sha256": digest.hexdigest(),
        "variable_count": len(names),
        "variable_names": names,
        "value_policy": "all inherited values affect the digest; only the explicit relevant-environment allowlist is stored",
    }


def toolchain_manifest(
    mimi: Path,
    *,
    probe_cli: bool = True,
    probe_toolchain_versions: bool = True,
    root: Path = ROOT,
    cargo_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    if cargo_authority is None:
        cargo_authority = cargo_test_toolchain_authority(
            root, probe_active_toolchain=probe_toolchain_versions
        )
    mimi_path = mimi.expanduser().resolve()
    binary: dict[str, Any] = {"path": str(mimi_path), "exists": mimi_path.is_file()}
    if mimi_path.is_file():
        binary["sha256"] = sha256_file(mimi_path)
        binary["bytes"] = mimi_path.stat().st_size
    if probe_cli and mimi_path.is_file():
        binary["version"] = _command_result([str(mimi_path), "--version"], cwd=root)
        build_help = _command_result([str(mimi_path), "build", "--help"], cwd=root)
        stats_help = _command_result([str(mimi_path), "stats", "--help"], cwd=root)
    else:
        binary["version_probe"] = "not_run_inventory_only" if not probe_cli else "binary_missing"
        build_help = {"status": "not_run_inventory_only" if not probe_cli else "binary_missing"}
        stats_help = {"status": "not_run_inventory_only" if not probe_cli else "binary_missing"}
    llvm_prefix = os.environ.get("LLVM_SYS_181_PREFIX")
    llvm_candidates: list[Path] = []
    if llvm_prefix:
        llvm_candidates.extend(
            [Path(llvm_prefix) / "bin" / "llvm-config", Path(llvm_prefix) / "llvm-config"]
        )
    llvm_from_path = shutil.which("llvm-config") or shutil.which("llvm-config-18")
    if llvm_from_path:
        llvm_candidates.append(Path(llvm_from_path))
    llvm_config = next((candidate for candidate in llvm_candidates if candidate.is_file()), None)
    llvm = (
        {
            **_tool_identity(str(llvm_config)),
            "version": _command_result([str(llvm_config), "--version"], cwd=root),
        }
        if llvm_config
        else {"status": "not_found", "candidates_checked": [str(p) for p in llvm_candidates]}
    )
    z3_identity = _tool_identity(os.environ.get("Z3", "z3"))
    z3_identity["version"] = (
        _command_result([z3_identity["path"], "--version"], cwd=root)
        if z3_identity.get("path")
        else {"status": "not_found"}
    )
    relevant_environment_names = (
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "LLVM_SYS_181_PREFIX",
        "LIBCLANG_PATH",
        "LD_LIBRARY_PATH",
        "PKG_CONFIG_PATH",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "RUSTC_WRAPPER",
        "CC",
        "CXX",
        "AR",
        "CFLAGS",
        "CXXFLAGS",
        "LDFLAGS",
        "CARGO_FEATURES",
        "MIMI_STDLIB",
        "MIMI_OPT",
        "MIMI_ASAN",
        "MIMI_EXEC_STRICT",
        "MIMI_RESOLVED_MODULE_BODIES",
        "MIMI_REACHABLE_DISPATCH",
        "MIMI_VALUE_GLUE",
        "MIMI_FFI_LIB",
        "MIMI_FFI_STRICT",
    )
    environment = {name: os.environ.get(name) for name in relevant_environment_names}
    environment["collector_overrides"] = {
        "MIMI_STAT": "1",
        "MIMI_VERBOSE": "1",
        "MIMI_ROUTE_CENSUS": "1",
        "MIMI_REACHABLE_DISPATCH": "unset (all non-comptime functions in stats)",
        "MIMI_RESOLVED_MODULE_BODIES": "unset (production default eligibility)",
        "MIMI_STAT_OUT": "unique per-input scratch directory",
        "TMPDIR": "unique per-input scratch directory",
    }
    cargo_identity = cargo_authority.get("cargo", {})
    rustc_identity = cargo_authority.get("rustc", {})
    rustup_toolchain = cargo_authority.get("rustup_toolchain", {})
    selected_cargo = (
        rustup_toolchain.get("selected_cargo", {})
        if isinstance(rustup_toolchain, Mapping)
        else {}
    )
    selected_rustc = (
        rustup_toolchain.get("selected_rustc", {})
        if isinstance(rustup_toolchain, Mapping)
        else {}
    )
    cargo_authority_current = bool(
        probe_toolchain_versions
        and _cargo_toolchain_identity_valid(cargo_authority, root=root)
    )
    cargo_tool = {
        **dict(cargo_identity),
        "selected_toolchain_binary": dict(selected_cargo),
        "version": (
            _command_result([str(selected_cargo["path"]), "--version"], cwd=root)
            if cargo_authority_current and selected_cargo.get("path")
            else {
                "status": "not_run_inventory_only"
                if not probe_toolchain_versions
                else "not_run_invalid_cargo_test_authority"
            }
        ),
        "authority_identity_sha256": cargo_authority.get("identity_sha256"),
    }
    rustc_tool = {
        **dict(rustc_identity),
        "selected_toolchain_binary": dict(selected_rustc),
        "version": (
            _command_result(
                [str(selected_rustc["path"]), "--version", "--verbose"], cwd=root
            )
            if cargo_authority_current and selected_rustc.get("path")
            else {
                "status": "not_run_inventory_only"
                if not probe_toolchain_versions
                else "not_run_invalid_cargo_test_authority"
            }
        ),
        "authority_identity_sha256": cargo_authority.get("identity_sha256"),
    }
    return {
        "platform": platform.platform(),
        "host": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "processor": platform.processor(),
        },
        "python": sys.version,
        "python_executable": {
            "path": str(Path(sys.executable).resolve()),
            "sha256": sha256_file(Path(sys.executable).resolve())
            if Path(sys.executable).is_file()
            else None,
        },
        "mimi": binary,
        "rustc": rustc_tool,
        "cargo": cargo_tool,
        "cargo_test_authority": dict(cargo_authority),
        "cc": {
            **_tool_identity(os.environ.get("CC", "cc")),
            "version": _command_result([os.environ.get("CC", "cc"), "--version"], cwd=root),
        },
        "llvm_config": llvm,
        "z3": z3_identity,
        "binary_build_features": {
            "reported_by_binary": "unknown; this CLI does not expose its compile-time Cargo feature set",
            "cargo_feature_environment": os.environ.get("CARGO_FEATURES"),
        },
        "mimi_build_help": build_help,
        "mimi_stats_help": stats_help,
        "relevant_environment": environment,
        "inherited_environment_fingerprint": _environment_fingerprint(),
        "observability_note": (
            "Canonical MIR builds report each verified MIR owner and reconcile against the route "
            "MIR digest; resolved-native builds use MIMI_STAT=1 totals plus MIMI_VERBOSE named events."
        ),
    }


def repository_identity(root: Path) -> dict[str, Any]:
    result: dict[str, Any] = {"git_commit": None, "tracked_diff_sha256": None, "git_status": None}
    commands = {
        "git_commit": ["git", "rev-parse", "HEAD"],
        "diff": ["git", "diff", "--binary", "HEAD", "--"],
        "status": ["git", "status", "--porcelain=v1", "-z"],
    }
    try:
        commit = subprocess.run(
            commands["git_commit"], cwd=root, capture_output=True, check=False
        )
        result["git_commit"] = _decode(commit.stdout).strip() if commit.returncode == 0 else None
        diff = subprocess.run(commands["diff"], cwd=root, capture_output=True, check=False)
        result["tracked_diff_sha256"] = sha256_bytes(diff.stdout) if diff.returncode == 0 else None
        status = subprocess.run(commands["status"], cwd=root, capture_output=True, check=False)
        if status.returncode == 0:
            entries = [entry for entry in status.stdout.split(b"\0") if entry]
            result["git_status"] = {
                "dirty": bool(entries),
                "entry_count": len(entries),
                "porcelain_sha256": sha256_bytes(status.stdout),
            }
    except OSError as error:
        result["git_error"] = str(error)
    return result


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def _is_sha256(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"[0-9a-f]{64}", value))


def _test_authority_is_structurally_valid(
    test_id: Any, source_text: str, lines: Sequence[str], line_number: Any
) -> tuple[bool, bool]:
    """Check that authority names a test declaration and cites an assertion-shaped line.

    Expected rejections receive an additional command/path/diagnostic check in
    ``_expected_rejection_test_authority_is_valid``.
    """
    if not isinstance(test_id, str) or not test_id.strip():
        return False, False
    declaration = re.compile(
        TEST_DECLARATION_TEMPLATE.format(name=re.escape(test_id))
    )
    test_id_present = bool(declaration.search(source_text))
    assertion_line_valid = (
        isinstance(line_number, int)
        and not isinstance(line_number, bool)
        and 1 <= line_number <= len(lines)
    )
    if not assertion_line_valid:
        return test_id_present, False
    line = lines[line_number - 1].lstrip()
    is_comment = line.startswith(("#", "//", "/*", "*"))
    return test_id_present, not is_comment and bool(ASSERTION_MARKER_RE.search(line))


def _find_rust_delimiter_end(text: str, opening_index: int, opening: str, closing: str) -> int | None:
    """Return the matching Rust delimiter, skipping strings and comments."""
    depth = 0
    index = opening_index
    while index < len(text):
        if text.startswith("//", index):
            newline = text.find("\n", index + 2)
            index = len(text) if newline < 0 else newline + 1
            continue
        if text.startswith("/*", index):
            comment_depth = 1
            index += 2
            while index < len(text) and comment_depth:
                if text.startswith("/*", index):
                    comment_depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    comment_depth -= 1
                    index += 2
                else:
                    index += 1
            continue
        raw_string = re.match(r"r(#{0,})\"", text[index:])
        if raw_string:
            terminator = '"' + raw_string.group(1)
            end = text.find(terminator, index + len(raw_string.group(0)))
            if end < 0:
                return None
            index = end + len(terminator)
            continue
        char = text[index]
        if char == '"':
            index += 1
            while index < len(text):
                if text[index] == "\\":
                    index += 2
                elif text[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            continue
        if char == "'":
            # Skip a bounded char literal while leaving Rust lifetimes alone.
            literal_end = index + 1
            if literal_end < len(text) and text[literal_end] == "\\":
                literal_end += 2
            else:
                literal_end += 1
            if literal_end < len(text) and text[literal_end] == "'":
                index = literal_end + 1
                continue
        if char == opening:
            depth += 1
        elif char == closing:
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    return None


def _mask_rust_comments_and_strings(text: str) -> str:
    """Blank Rust comments and literals while preserving offsets and line breaks."""
    masked = list(text)
    raw_string_prefix = re.compile(r"(?:br|cr|r)(#{0,})\"")

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if masked[position] not in "\r\n":
                masked[position] = " "

    index = 0
    while index < len(text):
        if text.startswith("//", index):
            newline = text.find("\n", index + 2)
            end = len(text) if newline < 0 else newline
            blank(index, end)
            index = end
            continue
        if text.startswith("/*", index):
            comment_start = index
            depth = 1
            index += 2
            while index < len(text) and depth:
                if text.startswith("/*", index):
                    depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            blank(comment_start, index)
            continue

        raw_string = (
            raw_string_prefix.match(text, index) if text[index] in "brc" else None
        )
        if raw_string:
            terminator = '"' + raw_string.group(1)
            end = text.find(terminator, index + len(raw_string.group(0)))
            end = len(text) if end < 0 else end + len(terminator)
            blank(index, end)
            index = end
            continue

        char = text[index]
        if char == '"':
            literal_start = index
            index += 1
            while index < len(text):
                if text[index] == "\\":
                    index += 2
                elif text[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            blank(literal_start, min(index, len(text)))
            continue
        if char == "'":
            # Mask bounded char literals, but leave Rust lifetimes intact.
            literal_end = index + 1
            if literal_end < len(text) and text[literal_end] == "\\":
                literal_end += 2
            elif literal_end < len(text) and text[literal_end] not in "\r\n'":
                literal_end += 1
            if literal_end < len(text) and text[literal_end] == "'":
                blank(index, literal_end + 1)
                index = literal_end + 1
                continue
        index += 1
    return "".join(masked)


def _rust_call_argument_slices(
    source_text: str,
    masked_text: str,
    start: int,
    end: int,
    function_name: str,
) -> Iterable[tuple[list[str], int]]:
    """Yield source argument spellings and offsets for syntactic Rust calls."""
    pattern = re.compile(rf"\b{re.escape(function_name)}[ \t\r\n]*\(")
    for match in pattern.finditer(masked_text, start, end):
        opening = match.end() - 1
        closing_end = _find_rust_delimiter_end(masked_text, opening, "(", ")")
        if closing_end is None or closing_end > end:
            continue
        if not masked_text[closing_end:end].lstrip(" \t\r\n").startswith(";"):
            continue
        arguments_start = opening + 1
        arguments_end = closing_end - 1
        argument_start = arguments_start
        paren_depth = bracket_depth = brace_depth = 0
        arguments: list[str] = []
        for position in range(arguments_start, arguments_end):
            char = masked_text[position]
            if char == "(":
                paren_depth += 1
            elif char == ")":
                paren_depth -= 1
            elif char == "[":
                bracket_depth += 1
            elif char == "]":
                bracket_depth -= 1
            elif char == "{":
                brace_depth += 1
            elif char == "}":
                brace_depth -= 1
            elif char == "," and not (paren_depth or bracket_depth or brace_depth):
                arguments.append(source_text[argument_start:position].strip())
                argument_start = position + 1
        trailing = source_text[argument_start:arguments_end].strip()
        if trailing or arguments:
            arguments.append(trailing)
        yield arguments, match.start()


def _rust_outer_attributes_before(code_text: str, item_start: int) -> list[str] | None:
    """Read the contiguous outer attributes immediately before an item.

    Rust permits arbitrary whitespace, including blank lines, between an
    attribute and its item. ``code_text`` must already have comments and
    literals masked so delimiters inside them cannot affect this scan.
    """
    attributes: list[str] = []
    cursor = item_start
    while True:
        while cursor > 0 and code_text[cursor - 1].isspace():
            cursor -= 1
        if cursor == 0 or code_text[cursor - 1] != "]":
            break

        closing = cursor - 1
        depth = 1
        opening = closing - 1
        while opening >= 0 and depth:
            if code_text[opening] == "]":
                depth += 1
            elif code_text[opening] == "[":
                depth -= 1
            opening -= 1
        if depth != 0:
            return None
        opening += 1
        hash_index = opening - 1
        if hash_index < 0 or code_text[hash_index] != "#":
            break
        attributes.append(code_text[hash_index:closing + 1])
        cursor = hash_index

    attributes.reverse()
    return attributes


def _rust_delimiter_depth_at(text: str, start: int, stop: int) -> tuple[int, int, int] | None:
    """Return brace/paren/bracket depth at ``stop`` within masked Rust text."""
    stack: list[str] = []
    closing = {"}": "{", ")": "(", "]": "["}
    for char in text[start:stop]:
        if char in "{([":
            stack.append(char)
        elif char in closing:
            if not stack or stack.pop() != closing[char]:
                return None
    return (
        stack.count("{"),
        stack.count("("),
        stack.count("["),
    )


def _rust_test_function_span(source_text: str, test_id: str) -> tuple[int, int] | None:
    """Return one provably active, nonignored top-level test function span."""
    declaration = re.compile(
        r"(?m)^[ \t]*(?:(?:pub(?:\([^)]*\))?|async|unsafe)[ \t]+)*"
        rf"fn[ \t]+{re.escape(test_id)}[ \t]*\([^)]*\)[^{{;]*\{{"
    )
    code_text = _mask_rust_comments_and_strings(source_text)
    matches = list(declaration.finditer(code_text))
    if len(matches) != 1:
        return None
    match = matches[0]

    # Actor authority lives at file scope. Requiring depth zero also prevents
    # matching declarations written as tokens inside a macro or nested item.
    if _rust_delimiter_depth_at(code_text, 0, match.start()) != (0, 0, 0):
        return None
    if re.search(
        r"(?m)^\s*#!\s*\[\s*(?:cfg|cfg_attr)\s*\(",
        code_text[:match.start()],
    ):
        # A crate-level conditional attribute can deactivate an otherwise
        # ordinary-looking test declaration. Do not infer the active cfg set.
        return None

    attributes = _rust_outer_attributes_before(code_text, match.start())
    if attributes is None:
        return None
    normalized = [re.sub(r"\s+", "", attribute) for attribute in attributes]
    if normalized != ["#[test]"]:
        # Only the ordinary test harness semantics are accepted. Attributes
        # such as should_panic, ignore, cfg, or custom harness modifiers can
        # change whether a failing test counts as passing or runs at all.
        return None

    opening_index = match.end() - 1
    end = _find_rust_delimiter_end(code_text, opening_index, "{", "}")
    if end is None:
        return None
    return match.start(), end


def _rust_named_function_span(
    source_text: str, function_name: str, *, require_test: bool = False
) -> tuple[int, int] | None:
    """Find one top-level Rust function, optionally requiring an active test."""
    declaration = re.compile(
        r"(?m)^[ \t]*(?:(?:pub(?:\([^)]*\))?|async|unsafe)[ \t]+)*"
        rf"fn[ \t]+{re.escape(function_name)}[ \t]*\([^)]*\)[^{{;]*\{{"
    )
    code_text = _mask_rust_comments_and_strings(source_text)
    matches = list(declaration.finditer(code_text))
    if len(matches) != 1:
        return None
    match = matches[0]
    if _rust_delimiter_depth_at(code_text, 0, match.start()) != (0, 0, 0):
        return None
    attributes = _rust_outer_attributes_before(code_text, match.start())
    if attributes is None:
        return None
    normalized = [re.sub(r"\s+", "", attribute) for attribute in attributes]
    if require_test:
        if normalized.count("#[test]") != 1:
            return None
    elif attributes:
        # The helper is compiled as an ordinary top-level item. Any attribute
        # could conditionally replace or disable the implementation.
        return None
    if any(
        re.match(r"#\[(?:ignore)(?:=|\(|\])", attribute)
        or re.match(r"#\[(?:cfg|cfg_attr)\(", attribute)
        for attribute in normalized
    ):
        return None
    end = _find_rust_delimiter_end(code_text, match.end() - 1, "{", "}")
    return (match.start(), end) if end is not None else None


def _actor_helper_has_required_contract(source_text: str, span: tuple[int, int]) -> bool:
    """Require the helper's active CLI invocations and result assertions."""
    tokens = _rust_tokens(source_text, span[0], span[1])

    def has(pattern: Sequence[tuple[str, str | None]]) -> bool:
        width = len(pattern)
        return any(
            _token_pattern_matches(tokens[index : index + width], pattern)
            for index in range(len(tokens) - width + 1)
        )

    required_patterns: tuple[tuple[tuple[str, str | None], ...], ...] = (
        (
            ("ident", "let"), ("ident", "source"), ("punct", "="),
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "path"), ("punct", "::"), ("ident", "Path"),
            ("punct", "::"), ("ident", "new"), ("punct", "("),
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "env"), ("punct", "!"), ("punct", "("),
            ("string", "CARGO_MANIFEST_DIR"), ("punct", ")"),
            ("punct", ")"), ("punct", "."), ("ident", "join"), ("punct", "("),
            ("string", "tests/real_world"), ("punct", ")"),
            ("punct", "."), ("ident", "join"), ("punct", "("),
            ("ident", "fixture"), ("punct", ")"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "process"), ("punct", "::"), ("ident", "Command"),
            ("punct", "::"), ("ident", "new"), ("punct", "("),
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "env"), ("punct", "!"), ("punct", "("),
            ("string", "CARGO_BIN_EXE_mimi"), ("punct", ")"), ("punct", ")"),
            ("punct", "."),
            ("ident", "arg"), ("punct", "("), ("string", "mir"),
            ("punct", ")"), ("punct", "."), ("ident", "arg"),
            ("punct", "("), ("punct", "&"), ("ident", "source"),
            ("punct", ")"), ("punct", "."), ("ident", "arg"),
            ("punct", "("), ("string", "--receipt"), ("punct", ")"),
            ("punct", "."), ("ident", "output"), ("punct", "("),
            ("punct", ")"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert"), ("punct", "!"), ("punct", "("),
            ("ident", "receipt"), ("punct", "."), ("ident", "status"),
            ("punct", "."), ("ident", "success"), ("punct", "("),
            ("punct", ")"),
        ),
        (
            ("ident", "for"), ("ident", "explicit"), ("ident", "in"),
            ("punct", "["), ("ident", "false"), ("punct", ","),
            ("ident", "true"), ("punct", "]"),
        ),
        (
            ("ident", "for"), ("punct", "("),
            ("ident", "command_index"), ("punct", ","),
            ("ident", "command"), ("punct", ")"), ("ident", "in"),
            ("punct", "["), ("string", "run"), ("punct", ","),
            ("string", "build"), ("punct", ","), ("string", "verify"),
            ("punct", "]"), ("punct", "."), ("ident", "into_iter"),
            ("punct", "("), ("punct", ")"), ("punct", "."),
            ("ident", "enumerate"), ("punct", "("), ("punct", ")"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert"), ("punct", "!"), ("punct", "("),
            ("ident", "observed"), ("punct", "."), ("ident", "status"),
            ("punct", "."), ("ident", "success"), ("punct", "("),
            ("punct", ")"),
        ),
        (
            ("ident", "stderr"), ("punct", "."), ("ident", "contains"),
            ("punct", "("), ("string", "canonical route disposition: legacy"),
            ("punct", ")"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert_eq"), ("punct", "!"), ("punct", "("),
            ("ident", "observed"), ("punct", "."), ("ident", "stdout"),
            ("punct", ","), ("ident", "expected"),
        ),
        (
            ("ident", "stdout"), ("punct", "."), ("ident", "contains"),
            ("punct", "("), ("string", "No contracts to verify"), ("punct", ")"),
        ),
        (
            ("ident", "stdout"), ("punct", "."), ("ident", "contains"),
            ("punct", "("), ("string", "Proven"), ("punct", ")"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "process"), ("punct", "::"), ("ident", "Command"),
            ("punct", "::"), ("ident", "new"),
            ("punct", "("), ("punct", "&"), ("ident", "binary"),
            ("punct", ")"), ("punct", "."), ("ident", "output"),
            ("punct", "("), ("punct", ")"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert_eq"), ("punct", "!"), ("punct", "("),
            ("ident", "native"), ("punct", "."), ("ident", "stdout"),
            ("punct", ","), ("ident", "expected"),
        ),
        (
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "process"), ("punct", "::"), ("ident", "Command"),
            ("punct", "::"), ("ident", "new"), ("punct", "("),
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "env"), ("punct", "!"), ("punct", "("),
            ("string", "CARGO_BIN_EXE_mimi"), ("punct", ")"), ("punct", ")"),
            ("punct", "."),
            ("ident", "env"), ("punct", "("),
            ("string", "MIMI_ROUTE_CENSUS"), ("punct", ","),
            ("string", "1"), ("punct", ")"), ("punct", "."),
            ("ident", "arg"), ("punct", "("), ("string", "disasm"),
            ("punct", ")"), ("punct", "."), ("ident", "arg"),
            ("punct", "("), ("punct", "&"), ("ident", "source"),
            ("punct", ")"), ("punct", "."), ("ident", "output"),
            ("punct", "("), ("punct", ")"),
        ),
        *tuple(
            (
                ("ident", "opcodes"), ("punct", "."), ("ident", "contains"),
                ("punct", "("), ("string", opcode), ("punct", ")"),
            )
            for opcode in ("MIR_ACTOR_SPAWN", "MIR_ACTOR_CALL", "MIR_ACTOR_FIELD_STORE")
        ),
    )
    return all(has(pattern) for pattern in required_patterns)


def actor_fixture_test_contracts(root: Path) -> dict[str, dict[str, Any]]:
    """Bind each required Actor fixture to an active test's exact helper call."""
    authority_path = ACTOR_HELPER_TEST_PATH
    authority_file = root / authority_path
    try:
        authority_bytes = authority_file.read_bytes()
        authority_text = authority_bytes.decode("utf-8")
        authority_digest = sha256_bytes(authority_bytes)
    except (OSError, UnicodeDecodeError):
        authority_text = ""
        authority_digest = None
        authority_bytes = b""
    helper_span = _rust_named_function_span(
        authority_text, ACTOR_HELPER_NAME
    )
    helper_digest = (
        sha256_bytes(authority_text[helper_span[0] : helper_span[1]].encode("utf-8"))
        if helper_span is not None
        else None
    )
    helper_valid = bool(
        helper_span is not None
        and helper_digest == ACTOR_HELPER_SHA256
        and _actor_helper_has_required_contract(authority_text, helper_span)
    )
    contracts: dict[str, dict[str, Any]] = {}
    for source_path in ACTOR_FALLBACK_INPUTS:
        test_id = ACTOR_AUTHORITY_TEST_IDS[source_path]
        expected_stdout = ACTOR_EXPECTED_STDOUT[source_path]
        span = _rust_test_function_span(authority_text, test_id)
        assertion: dict[str, Any] | None = None
        if span is not None and authority_digest is not None and helper_valid:
            function_start, function_end = span
            code_text = _mask_rust_comments_and_strings(authority_text)
            body_open = code_text.find("{", function_start, function_end)
            body_start = body_open + 1 if body_open >= 0 else function_end
            body_end = function_end - 1
            body_text = code_text[body_start:body_end]
            fixture_literal = json.dumps(Path(source_path).name)
            stdout_literal = json.dumps(expected_stdout.decode("ascii"))
            expected_arguments = [fixture_literal, "b" + stdout_literal, "false"]
            calls = list(
                _rust_call_argument_slices(
                    authority_text,
                    code_text,
                    body_start,
                    body_end,
                    "assert_original_actor_cli_routes",
                )
            )

            def is_top_level_unconditional_call(call_start: int) -> bool:
                call_attributes = _rust_outer_attributes_before(code_text, call_start)
                return (
                    _rust_delimiter_depth_at(code_text, body_start, call_start)
                    == (0, 0, 0)
                    and call_attributes is not None
                    and not any(
                        re.match(r"#\[(?:cfg|cfg_attr)\(", re.sub(r"\s+", "", attr))
                        for attr in call_attributes
                    )
                )

            has_conditional_body_attributes = bool(
                re.search(r"#\s*\[\s*(?:cfg|cfg_attr)\s*\(", body_text)
            )
            matching_calls = [] if has_conditional_body_attributes else [
                call_start
                for arguments, call_start in calls
                if arguments == expected_arguments
                and is_top_level_unconditional_call(call_start)
            ]
            if len(calls) == 1 and len(matching_calls) == 1:
                matching_call = matching_calls[0]
                opening = code_text.find("(", matching_call)
                closing_end = (
                    _find_rust_delimiter_end(code_text, opening, "(", ")")
                    if opening >= 0
                    else None
                )
                statement_end = closing_end
                if statement_end is not None:
                    while statement_end < body_end and code_text[statement_end].isspace():
                        statement_end += 1
                    if statement_end < body_end and code_text[statement_end] == ";":
                        statement_end += 1
                    else:
                        statement_end = None
                # Require the cited call to be the test's only direct body
                # statement. This excludes unreachable calls after `return`,
                # setup that could divert control, and calls merely nested in
                # another item or expression.
                if (
                    statement_end is not None
                    and not code_text[body_start:matching_call].strip()
                    and not code_text[statement_end:body_end].strip()
                ):
                    line = authority_text.count("\n", 0, matching_call) + 1
                    assertion = {
                        "path": authority_path,
                        "line": line,
                        "sha256": authority_digest,
                    }
        contracts[source_path] = {
            "test_id": test_id,
            "authority_valid": assertion is not None,
            "helper_name": ACTOR_HELPER_NAME,
            "helper_sha256": helper_digest,
            "helper_sha256_expected": ACTOR_HELPER_SHA256,
            "helper_valid": helper_valid,
            "authority": {
                "test_id": test_id,
                "assertion": assertion,
            } if assertion is not None else None,
            "expected_stdout": _raw_capture(expected_stdout),
        }
    return contracts


def _regular_file_sha256(path: Path) -> str | None:
    try:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode):
            return None
        return sha256_file(path)
    except OSError:
        return None


def _actor_test_pass_output(stdout: bytes, test_id: str) -> bool:
    observed = re.findall(
        rb"(?m)^test ([A-Za-z_][A-Za-z0-9_]*) \.\.\. (ok|FAILED|ignored)$",
        stdout,
    )
    running = re.findall(rb"(?m)^running ([0-9]+) tests?$", stdout)
    summaries = re.findall(
        rb"(?m)^test result: ok\. 1 passed; 0 failed;[^\r\n]*$", stdout
    )
    return (
        observed == [(test_id.encode("ascii"), b"ok")]
        and running == [b"1"]
        and len(summaries) == 1
    )


def _actor_test_execution_valid(
    result: Any,
    source_path: str,
    source: Mapping[str, Any] | None,
    contract: Mapping[str, Any] | None,
    *,
    root: Path,
    logs_root: Path | None = None,
    cargo_authority: Mapping[str, Any] | None = None,
) -> bool:
    """Validate one exact Cargo test result against its fixture and authority hashes."""
    if source_path not in ACTOR_AUTHORITY_TEST_IDS or not isinstance(result, Mapping):
        return False
    test_id = ACTOR_AUTHORITY_TEST_IDS[source_path]
    authority = contract.get("authority") if isinstance(contract, Mapping) else None
    assertion = authority.get("assertion") if isinstance(authority, Mapping) else None
    expected_fixture_hash = source.get("sha256") if isinstance(source, Mapping) else None
    expected_test_hash = assertion.get("sha256") if isinstance(assertion, Mapping) else None
    cargo_path = cargo_authority.get("cargo", {}).get("path") if isinstance(cargo_authority, Mapping) else None
    expected_command = [
        cargo_path, "test", "--test", "real_world_cli", test_id, "--", "--exact"
    ]
    stdout = _captured_bytes(result.get("stdout_raw"))
    stderr = _captured_bytes(result.get("stderr_raw"))
    if logs_root is not None and not all(
        _raw_sidecar_matches(logs_root, result.get(key))
        for key in ("stdout_raw", "stderr_raw")
    ):
        return False
    return bool(
        isinstance(source, Mapping)
        and source.get("path") == source_path
        and source.get("present") is True
        and source.get("presence_kind") == "regular_file"
        and _is_sha256(expected_fixture_hash)
        and isinstance(contract, Mapping)
        and contract.get("authority_valid") is True
        and contract.get("helper_valid") is True
        and contract.get("helper_sha256") == ACTOR_HELPER_SHA256
        and result.get("helper_sha256_expected") == ACTOR_HELPER_SHA256
        and result.get("helper_sha256_observed") == ACTOR_HELPER_SHA256
        and contract.get("helper_name") == ACTOR_HELPER_NAME
        and contract.get("helper_sha256") == ACTOR_HELPER_SHA256
        and contract.get("helper_valid") is True
        and isinstance(authority, Mapping)
        and isinstance(assertion, Mapping)
        and authority.get("test_id") == test_id
        and assertion.get("path") == "tests/real_world_cli.rs"
        and _is_sha256(expected_test_hash)
        and result.get("path") == source_path
        and result.get("test_id") == test_id
        and result.get("authority_path") == "tests/real_world_cli.rs"
        and result.get("command") == expected_command
        and _cargo_execution_evidence_valid(
            result.get("cargo_execution"), cargo_authority, root=root
        )
        and result.get("cwd") == str(root)
        and result.get("run_status") == "completed"
        and result.get("exit_code") == 0
        and result.get("fixture_sha256_expected") == expected_fixture_hash
        and result.get("fixture_sha256_before") == expected_fixture_hash
        and result.get("fixture_sha256_after") == expected_fixture_hash
        and result.get("test_source_sha256_expected") == expected_test_hash
        and result.get("test_source_sha256_before") == expected_test_hash
        and result.get("test_source_sha256_after") == expected_test_hash
        and stdout is not None
        and stderr is not None
        and _actor_test_pass_output(stdout, test_id)
    )


def _run_actor_authority_tests(
    root: Path,
    logs_root: Path,
    sources: Sequence[Mapping[str, Any]],
    actor_contracts: Mapping[str, Mapping[str, Any]],
    timeout: float,
    cargo_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Run only the two named Actor tests and retain hash-bound raw logs."""
    source_by_path = {str(source.get("path")): source for source in sources}
    if cargo_authority is None:
        cargo_authority = cargo_test_toolchain_authority(root)
    authority_path = ACTOR_HELPER_TEST_PATH
    authority_file = root / authority_path
    results: list[dict[str, Any]] = []
    for index, source_path in enumerate(ACTOR_FALLBACK_INPUTS, 1):
        test_id = ACTOR_AUTHORITY_TEST_IDS[source_path]
        source = source_by_path.get(source_path)
        contract = actor_contracts.get(source_path, {})
        fixture_file = root / source_path
        fixture_before = _regular_file_sha256(fixture_file)
        test_before = _regular_file_sha256(authority_file)
        cargo_path = cargo_authority.get("cargo", {}).get("path")
        command = [
            cargo_path, "test", "--test", "real_world_cli", test_id, "--", "--exact"
        ]
        run_status = "completed"
        exit_code: int | None
        stdout = b""
        stderr = b""
        execution: dict[str, Any] | None = None
        if not (root / "Cargo.toml").is_file():
            run_status = "cargo_project_missing"
            exit_code = None
            stderr = b"Cargo.toml is absent; exact Actor integration tests were not run\n"
        else:
            execution = _run_cargo_authority_command(
                command, root, timeout, cargo_authority
            )
            run_status = str(execution["run_status"])
            exit_code = execution["exit_code"]
            stdout = execution["stdout_bytes"]
            stderr = execution["stderr_bytes"]
        fixture_after = _regular_file_sha256(fixture_file)
        test_after = _regular_file_sha256(authority_file)
        stem = f"actor-test-{index:02d}"
        stdout_raw = _write_raw_artifact(logs_root / f"{stem}.stdout.bin", stdout)
        stderr_raw = _write_raw_artifact(logs_root / f"{stem}.stderr.bin", stderr)
        authority = contract.get("authority") if isinstance(contract, Mapping) else None
        assertion = authority.get("assertion") if isinstance(authority, Mapping) else None
        result = {
            "path": source_path,
            "test_id": test_id,
            "authority_path": authority_path,
            "command": command,
            "cargo_toolchain_identity_sha256": cargo_authority.get("identity_sha256"),
            "cargo_execution": execution.get("cargo_execution") if execution is not None else None,
            "cwd": str(root),
            "environment_policy": "inherits only after rejecting compiler/cfg/target/runner overrides; collector pins RUSTC to its resolved absolute path",
            "run_status": run_status,
            "exit_code": exit_code,
            "fixture_sha256_expected": source.get("sha256") if isinstance(source, Mapping) else None,
            "fixture_sha256_before": fixture_before,
            "fixture_sha256_after": fixture_after,
            "test_source_sha256_expected": assertion.get("sha256") if isinstance(assertion, Mapping) else None,
            "test_source_sha256_before": test_before,
            "test_source_sha256_after": test_after,
            "helper_sha256_expected": contract.get("helper_sha256_expected")
            if isinstance(contract, Mapping)
            else None,
            "helper_sha256_observed": contract.get("helper_sha256")
            if isinstance(contract, Mapping)
            else None,
            "stdout": _decode(stdout),
            "stderr": _decode(stderr),
            "stdout_raw": stdout_raw,
            "stderr_raw": stderr_raw,
        }
        result["execution_valid"] = _actor_test_execution_valid(
            result,
            source_path,
            source,
            contract,
            root=root,
            logs_root=logs_root,
            cargo_authority=cargo_authority,
        )
        results.append(result)
    expected_pairs = {
        (path, ACTOR_AUTHORITY_TEST_IDS[path]) for path in ACTOR_FALLBACK_INPUTS
    }
    observed_pairs = {
        (str(result.get("path")), str(result.get("test_id"))) for result in results
    }
    passed = (
        len(results) == len(ACTOR_FALLBACK_INPUTS)
        and observed_pairs == expected_pairs
        and all(result.get("execution_valid") is True for result in results)
    )
    return {
        "status": "passed" if passed else "failed",
        "required_tests": [
            {"path": path, "test_id": ACTOR_AUTHORITY_TEST_IDS[path]}
            for path in ACTOR_FALLBACK_INPUTS
        ],
        "command_template": [
            "<cargo-absolute-path>", "test", "--test", "real_world_cli", "<exact-test-id>", "--", "--exact"
        ],
        "cases": results,
    }


def _rust_tokens(text: str, start: int = 0, end: int | None = None) -> list[tuple[str, str, int, int]]:
    """Tokenize the small Rust surface used by authority contracts.

    Comments disappear and string literals become indivisible tokens. This is
    intentionally not a Rust parser: callers accept a narrow statement shape
    and fail closed on syntax outside that shape.
    """
    stop = len(text) if end is None else end
    tokens: list[tuple[str, str, int, int]] = []
    index = start
    paired_punctuation = {"::", "&&", "||", "->", "=>", "==", "!=", "<=", ">="}
    while index < stop:
        if text[index].isspace():
            index += 1
            continue
        if text.startswith("//", index):
            newline = text.find("\n", index + 2, stop)
            index = stop if newline < 0 else newline + 1
            continue
        if text.startswith("/*", index):
            depth = 1
            index += 2
            while index < stop and depth:
                if text.startswith("/*", index):
                    depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            if depth:
                return []
            continue

        raw_prefix = re.match(r'(?:br|cr|r)(#{0,})"', text[index:stop])
        if raw_prefix:
            terminator = '"' + raw_prefix.group(1)
            content_start = index + len(raw_prefix.group(0))
            closing = text.find(terminator, content_start, stop)
            if closing < 0:
                return []
            token_end = closing + len(terminator)
            tokens.append(("string", text[content_start:closing], index, token_end))
            index = token_end
            continue

        if text[index] == '"':
            literal_start = index
            index += 1
            value: list[str] = []
            while index < stop:
                if text[index] == "\\":
                    if index + 1 >= stop:
                        return []
                    value.extend(text[index : index + 2])
                    index += 2
                elif text[index] == '"':
                    index += 1
                    break
                else:
                    value.append(text[index])
                    index += 1
            else:
                return []
            tokens.append(("string", "".join(value), literal_start, index))
            continue

        if text[index] == "'":
            # Keep lifetimes as punctuation plus an identifier. Bounded char
            # literals are one token so apostrophes cannot alter nesting.
            cursor = index + 1
            if cursor < stop and text[cursor] == "\\":
                cursor += 2
            elif cursor < stop and text[cursor] not in "\r\n'":
                cursor += 1
            if cursor < stop and text[cursor] == "'":
                tokens.append(("char", text[index + 1 : cursor], index, cursor + 1))
                index = cursor + 1
                continue

        if text[index].isalpha() or text[index] == "_":
            token_start = index
            index += 1
            while index < stop and (text[index].isalnum() or text[index] == "_"):
                index += 1
            tokens.append(("ident", text[token_start:index], token_start, index))
            continue

        pair = text[index : index + 2]
        if pair in paired_punctuation:
            tokens.append(("punct", pair, index, index + 2))
            index += 2
        else:
            tokens.append(("punct", text[index], index, index + 1))
            index += 1
    return tokens


def _rust_top_level_statements(
    text: str, start: int, end: int
) -> list[list[tuple[str, str, int, int]]] | None:
    tokens = _rust_tokens(text, start, end)
    statements: list[list[tuple[str, str, int, int]]] = []
    current: list[tuple[str, str, int, int]] = []
    paren_depth = bracket_depth = 0
    for token in tokens:
        value = token[1]
        if value in "{}":
            # Nested blocks, closures and brace-form macros are outside the
            # accepted authority shape, including an unreachable block.
            return None
        if value == "(":
            paren_depth += 1
        elif value == ")":
            paren_depth -= 1
            if paren_depth < 0:
                return None
        elif value == "[":
            bracket_depth += 1
        elif value == "]":
            bracket_depth -= 1
            if bracket_depth < 0:
                return None
        if value == ";" and paren_depth == 0 and bracket_depth == 0:
            if not current:
                return None
            statements.append(current)
            current = []
        else:
            current.append(token)
    if current or paren_depth or bracket_depth:
        return None
    return statements


def _token_pattern_matches(
    tokens: Sequence[tuple[str, str, int, int]],
    pattern: Sequence[tuple[str, str | None]],
) -> bool:
    return len(tokens) == len(pattern) and all(
        token[0] == kind and (value is None or token[1] == value)
        for token, (kind, value) in zip(tokens, pattern)
    )


def _expected_rejection_test_authority_is_valid(
    *,
    test_id: Any,
    source_path: str,
    target_kind: str,
    source_text: str,
    assertion_line: Any,
    expected_diagnostic_codes: Any,
    required_output_substrings: Any,
    expected_route_reason: Any,
) -> bool:
    """Require a direct, unconditional test body for this exact default build."""
    if (
        not isinstance(test_id, str)
        or Path(source_path).suffix != ".mimi"
        or target_kind not in {"executable", "shared_library_probe"}
        or not isinstance(assertion_line, int)
        or isinstance(assertion_line, bool)
        or not isinstance(expected_diagnostic_codes, list)
        or not expected_diagnostic_codes
        or not isinstance(required_output_substrings, list)
        or not required_output_substrings
        or not isinstance(expected_route_reason, str)
        or not expected_route_reason.strip()
    ):
        return False
    span = _rust_test_function_span(source_text, test_id)
    if span is None:
        return False
    function_start, function_end = span
    code_text = _mask_rust_comments_and_strings(source_text)
    body_open = code_text.find("{", function_start, function_end)
    if body_open < 0:
        return False
    body_start = body_open + 1
    body_end = function_end - 1
    statements = _rust_top_level_statements(source_text, body_start, body_end)
    if statements is None or len(statements) < 5:
        return False

    values = [[token[1] for token in statement] for statement in statements]
    first = statements[0]
    if len(values[0]) < 3 or values[0][0] != "let" or values[0][2] != "=":
        return False
    fixture_variable = values[0][1]
    fixture_prefix = [
        ("ident", "let"), ("ident", fixture_variable), ("punct", "="),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "path"), ("punct", "::"), ("ident", "Path"),
        ("punct", "::"), ("ident", "new"), ("punct", "("),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "env"), ("punct", "!"), ("punct", "("),
        ("string", "CARGO_MANIFEST_DIR"), ("punct", ")"), ("punct", ")"),
    ]
    if not _token_pattern_matches(first[:len(fixture_prefix)], fixture_prefix):
        return False
    joined_parts: list[str] = []
    cursor = len(fixture_prefix)
    while cursor < len(first):
        if values[0][cursor : cursor + 3] != [".", "join", "("]:
            return False
        literal_index = cursor + 3
        if (
            literal_index + 1 >= len(first)
            or first[literal_index][0] != "string"
            or values[0][literal_index + 1] != ")"
        ):
            return False
        joined_parts.append(values[0][literal_index])
        cursor = literal_index + 2
    if tuple(joined_parts) != tuple(Path(source_path).parts):
        return False

    command_statement = statements[1]
    if (
        values[1][0:1] != ["let"]
        or len(values[1]) < 3
        or command_statement[1][0] != "ident"
        or values[1][2] != "="
    ):
        return False
    command_variable = values[1][1]
    command_prefix = [
        ("ident", "let"), ("ident", command_variable), ("punct", "="),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "process"), ("punct", "::"), ("ident", "Command"),
        ("punct", "::"), ("ident", "new"), ("punct", "("),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "env"), ("punct", "!"), ("punct", "("),
        ("string", "CARGO_BIN_EXE_mimi"), ("punct", ")"), ("punct", ")"),
        ("punct", "."), ("ident", "current_dir"), ("punct", "("),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "env"), ("punct", "!"), ("punct", "("),
        ("string", "CARGO_MANIFEST_DIR"), ("punct", ")"),
        ("punct", ")"), ("punct", "."), ("ident", "arg"),
        ("punct", "("), ("string", "build"), ("punct", ")"),
    ]
    if not _token_pattern_matches(command_statement[: len(command_prefix)], command_prefix):
        return False
    cursor = len(command_prefix)
    if target_kind == "shared_library_probe":
        shared_pattern = [
            ("punct", "."), ("ident", "arg"), ("punct", "("),
            ("string", "--shared"), ("punct", ")"),
        ]
        if not _token_pattern_matches(
            command_statement[cursor : cursor + len(shared_pattern)], shared_pattern
        ):
            return False
        cursor += len(shared_pattern)
    command_suffix = [
        ("punct", "."), ("ident", "arg"), ("punct", "("),
        ("punct", "&"), ("ident", fixture_variable), ("punct", ")"),
        ("punct", "."), ("ident", "arg"), ("punct", "("),
        ("string", "-o"), ("punct", ")"),
        ("punct", "."), ("ident", "arg"), ("punct", "("),
        ("string", None), ("punct", ")"),
        ("punct", "."), ("ident", "output"), ("punct", "("),
        ("punct", ")"), ("punct", "."), ("ident", "expect"),
        ("punct", "("), ("string", None), ("punct", ")"),
    ]
    if not _token_pattern_matches(command_statement[cursor:], command_suffix):
        return False
    if not command_statement[cursor + 14][1] or not command_statement[cursor + 23][1]:
        return False

    status_pattern = [
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "assert"), ("punct", "!"), ("punct", "("),
        ("punct", "!"), ("ident", command_variable), ("punct", "."),
        ("ident", "status"), ("punct", "."), ("ident", "success"),
        ("punct", "("), ("punct", ")"), ("punct", ")"),
    ]
    if not _token_pattern_matches(statements[2], status_pattern):
        return False

    if not _token_pattern_matches(
        statements[3],
        [
            ("ident", "let"), ("ident", None), ("punct", "="),
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "string"), ("punct", "::"), ("ident", "String"),
            ("punct", "::"), ("ident", "from_utf8_lossy"), ("punct", "("),
            ("punct", "&"), ("ident", command_variable), ("punct", "."),
            ("ident", "stderr"), ("punct", ")"),
        ],
    ):
        return False
    stderr_variable = values[3][1]

    required_literals = {
        *[str(code).upper() for code in expected_diagnostic_codes],
        *[str(item) for item in required_output_substrings],
    }
    route_lines = {
        f"default Canonical MIR route rejected: {expected_route_reason}",
        f"canonical MIR build rejected: {expected_route_reason}",
    }
    asserted_literals: set[str] = set()
    cited_line_is_diagnostic_assertion = False
    for statement in statements[4:]:
        tokens = statement
        assert_prefix = [
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert"), ("punct", "!"), ("punct", "("),
        ]
        if len(tokens) < 11 or not _token_pattern_matches(
            tokens[:len(assert_prefix)], assert_prefix
        ):
            return False
        if tokens[-1][1] != ")":
            return False
        cursor = len(assert_prefix)
        statement_literals: set[str] = set()
        while cursor < len(tokens) - 1:
            contains_pattern = [
                ("ident", stderr_variable), ("punct", "."),
                ("ident", "contains"), ("punct", "("), ("string", None),
            ]
            if not _token_pattern_matches(tokens[cursor : cursor + 5], contains_pattern):
                return False
            if cursor + 5 >= len(tokens) - 1 or tokens[cursor + 5][1] != ")":
                return False
            literal = tokens[cursor + 4][1]
            statement_literals.add(literal)
            asserted_literals.add(literal)
            cursor += 6
            if cursor < len(tokens) - 1:
                if tokens[cursor][1] != "&&":
                    return False
                cursor += 1
        if not statement_literals:
            return False
        line_start = source_text.count("\n", 0, statement[0][2]) + 1
        line_end = source_text.count("\n", 0, statement[-1][3]) + 1
        if line_start <= assertion_line <= line_end:
            cited_line_is_diagnostic_assertion = True

    return bool(
        required_literals.issubset(asserted_literals)
        and route_lines.intersection(asserted_literals)
        and cited_line_is_diagnostic_assertion
    )


def _required_compile_test_authority_is_valid(
    *,
    test_id: Any,
    source_path: str,
    target_kind: str,
    source_text: str,
    assertion_line: Any,
) -> bool:
    """Require a direct successful default build of the exact cited Mimi input.

    The accepted Rust body is deliberately straight-line and narrow: it joins
    the cited repository-relative source beneath ``CARGO_MANIFEST_DIR``, runs
    Cargo's Mimi binary with the default ``build`` command, enables route
    telemetry, and asserts the canonical MIR route and native receipt markers.
    The exact test still has to be built and run by Cargo before it becomes
    effective authority.
    """
    if (
        not isinstance(test_id, str)
        or Path(source_path).suffix != ".mimi"
        or target_kind not in {"executable", "shared_library_probe"}
        or not isinstance(assertion_line, int)
        or isinstance(assertion_line, bool)
    ):
        return False
    span = _rust_test_function_span(source_text, test_id)
    if span is None:
        return False
    function_start, function_end = span
    code_text = _mask_rust_comments_and_strings(source_text)
    body_open = code_text.find("{", function_start, function_end)
    if body_open < 0:
        return False
    statements = _rust_top_level_statements(
        source_text, body_open + 1, function_end - 1
    )
    if statements is None or len(statements) < 10:
        return False
    values = [[token[1] for token in statement] for statement in statements]

    fixture_statement = statements[0]
    if len(values[0]) < 3 or values[0][0] != "let" or values[0][2] != "=":
        return False
    fixture_variable = values[0][1]
    fixture_prefix = [
        ("ident", "let"), ("ident", fixture_variable), ("punct", "="),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "path"), ("punct", "::"), ("ident", "Path"),
        ("punct", "::"), ("ident", "new"), ("punct", "("),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "env"), ("punct", "!"), ("punct", "("),
        ("string", "CARGO_MANIFEST_DIR"), ("punct", ")"), ("punct", ")"),
    ]
    if not _token_pattern_matches(fixture_statement[:len(fixture_prefix)], fixture_prefix):
        return False
    joined_parts: list[str] = []
    cursor = len(fixture_prefix)
    while cursor < len(fixture_statement):
        if values[0][cursor:cursor + 3] != [".", "join", "("]:
            return False
        literal_index = cursor + 3
        if (
            literal_index + 1 >= len(fixture_statement)
            or fixture_statement[literal_index][0] != "string"
            or values[0][literal_index + 1] != ")"
        ):
            return False
        joined_parts.append(values[0][literal_index])
        cursor = literal_index + 2
    if tuple(joined_parts) != tuple(Path(source_path).parts):
        return False

    command_statement = statements[1]
    if (
        values[1][0:1] != ["let"]
        or len(values[1]) < 3
        or command_statement[1][0] != "ident"
        or values[1][2] != "="
    ):
        return False
    output_variable = values[1][1]
    command_prefix = [
        ("ident", "let"), ("ident", output_variable), ("punct", "="),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "process"), ("punct", "::"), ("ident", "Command"),
        ("punct", "::"), ("ident", "new"), ("punct", "("),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "env"), ("punct", "!"), ("punct", "("),
        ("string", "CARGO_BIN_EXE_mimi"), ("punct", ")"), ("punct", ")"),
        ("punct", "."), ("ident", "current_dir"), ("punct", "("),
        ("punct", "::"), ("ident", "std"), ("punct", "::"),
        ("ident", "env"), ("punct", "!"), ("punct", "("),
        ("string", "CARGO_MANIFEST_DIR"), ("punct", ")"), ("punct", ")"),
        ("punct", "."), ("ident", "arg"),
        ("punct", "("), ("string", "build"), ("punct", ")"),
    ]
    if not _token_pattern_matches(command_statement[:len(command_prefix)], command_prefix):
        return False
    cursor = len(command_prefix)
    if target_kind == "shared_library_probe":
        shared_pattern = [
            ("punct", "."), ("ident", "arg"), ("punct", "("),
            ("string", "--shared"), ("punct", ")"),
        ]
        if not _token_pattern_matches(
            command_statement[cursor:cursor + len(shared_pattern)], shared_pattern
        ):
            return False
        cursor += len(shared_pattern)
    command_middle = [
        ("punct", "."), ("ident", "arg"), ("punct", "("),
        ("punct", "&"), ("ident", fixture_variable), ("punct", ")"),
        ("punct", "."), ("ident", "arg"), ("punct", "("),
        ("string", "-o"), ("punct", ")"),
        ("punct", "."), ("ident", "arg"), ("punct", "("),
        ("string", None), ("punct", ")"),
        ("punct", "."), ("ident", "env"), ("punct", "("),
        ("string", "MIMI_ROUTE_CENSUS"), ("punct", ","),
        ("string", "1"), ("punct", ")"),
        ("punct", "."), ("ident", "output"), ("punct", "("),
        ("punct", ")"), ("punct", "."), ("ident", "expect"),
        ("punct", "("), ("string", None), ("punct", ")"),
    ]
    if not _token_pattern_matches(command_statement[cursor:], command_middle):
        return False
    output_path_index = cursor + 14
    if not command_statement[output_path_index][1].strip():
        return False

    if not _token_pattern_matches(
        statements[2],
        [
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert"), ("punct", "!"), ("punct", "("),
            ("ident", output_variable), ("punct", "."), ("ident", "status"),
            ("punct", "."), ("ident", "success"), ("punct", "("),
            ("punct", ")"), ("punct", ")"),
        ],
    ):
        return False
    if not _token_pattern_matches(
        statements[3],
        [
            ("ident", "let"), ("ident", None), ("punct", "="),
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "string"), ("punct", "::"), ("ident", "String"),
            ("punct", "::"), ("ident", "from_utf8_lossy"), ("punct", "("),
            ("punct", "&"), ("ident", output_variable), ("punct", "."),
            ("ident", "stderr"), ("punct", ")"),
        ],
    ):
        return False
    stderr_variable = values[3][1]

    required_present = {
        "canonical route disposition: canonical (",
        "MIMI_ROUTE_CENSUS route-admission consumer=build mode=default ",
        "MIMI_ROUTE_CENSUS canonical-native-build consumer=build ",
        "mir_digest=",
        "receipt_identity_digest=",
    }
    required_absent = {
        "canonical route disposition: legacy",
        "canonical route disposition: compatibility",
        "fallback",
        "compatibility",
    }
    asserted_present: set[str] = set()
    asserted_absent: set[str] = set()
    cited_route_assertion = False
    for statement in statements[4:]:
        if len(statement) < 12:
            return False
        negative = statement[6][1] == "!"
        prefix = [
            ("punct", "::"), ("ident", "std"), ("punct", "::"),
            ("ident", "assert"), ("punct", "!"), ("punct", "("),
        ]
        if not _token_pattern_matches(statement[:len(prefix)], prefix):
            return False
        inner = statement[len(prefix):]
        if negative:
            if not _token_pattern_matches(
                inner,
                [
                    ("punct", "!"), ("ident", stderr_variable), ("punct", "."),
                    ("ident", "contains"), ("punct", "("), ("string", None),
                    ("punct", ")"), ("punct", ")"),
                ],
            ):
                return False
        else:
            if not _token_pattern_matches(
                inner,
                [
                    ("ident", stderr_variable), ("punct", "."),
                    ("ident", "contains"), ("punct", "("), ("string", None),
                    ("punct", ")"), ("punct", ")"),
                ],
            ):
                return False
        literal = inner[5 if negative else 4][1]
        if negative:
            if literal not in required_absent or literal in asserted_absent:
                return False
            asserted_absent.add(literal)
        else:
            if literal not in required_present or literal in asserted_present:
                return False
            asserted_present.add(literal)
        line_start = source_text.count("\n", 0, statement[0][2]) + 1
        line_end = source_text.count("\n", 0, statement[-1][3]) + 1
        if line_start <= assertion_line <= line_end:
            cited_route_assertion = True

    return bool(
        asserted_present == required_present
        and asserted_absent == required_absent
        and cited_route_assertion
    )


def _cargo_metadata(
    root: Path,
    timeout: float = 120.0,
    cargo_authority: Mapping[str, Any] | None = None,
) -> tuple[dict[str, Any], Any]:
    """Read Cargo's declared target graph without compiling project code."""
    if cargo_authority is None:
        cargo_authority = cargo_test_toolchain_authority(root)
    cargo_path = cargo_authority.get("cargo", {}).get("path")
    command = [cargo_path, "metadata", "--format-version", "1", "--no-deps"]
    execution = _run_cargo_authority_command(
        command, root, timeout, cargo_authority
    )
    run_status = execution["run_status"]
    exit_code = execution["exit_code"]
    stdout = execution["stdout_bytes"]
    stderr = execution["stderr_bytes"]
    result = {
        "command": command,
        "cwd": str(root),
        "run_status": run_status,
        "exit_code": exit_code,
        "cargo_toolchain_identity_sha256": cargo_authority.get("identity_sha256"),
        "cargo_execution": execution["cargo_execution"],
        "stdout_raw": _raw_capture(stdout),
        "stderr_raw": _raw_capture(stderr),
    }
    metadata: Any = None
    if run_status == "completed" and exit_code == 0:
        try:
            metadata = json.loads(stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            metadata = None
    return result, metadata


def _cargo_test_target_for_source(
    metadata: Any, root: Path, source_path: str
) -> dict[str, Any] | None:
    """Resolve a source only when Cargo declares it as an active test target."""
    if not isinstance(metadata, Mapping):
        return None
    members = metadata.get("workspace_members")
    packages = metadata.get("packages")
    if not isinstance(members, list) or not isinstance(packages, list):
        return None
    try:
        expected_source = (root / source_path).resolve(strict=True)
        root_resolved = root.resolve(strict=True)
        expected_source.relative_to(root_resolved)
    except (OSError, ValueError):
        return None
    matches: list[dict[str, Any]] = []
    for package in packages:
        if not isinstance(package, Mapping) or package.get("id") not in members:
            continue
        manifest = package.get("manifest_path")
        package_name = package.get("name")
        package_id = package.get("id")
        if not isinstance(manifest, str) or not isinstance(package_name, str) or not isinstance(package_id, str):
            continue
        try:
            manifest_path = Path(manifest).resolve(strict=True)
            manifest_path.relative_to(root_resolved)
        except (OSError, ValueError):
            continue
        targets = package.get("targets")
        if not isinstance(targets, list):
            continue
        for target in targets:
            if not isinstance(target, Mapping):
                continue
            kinds = target.get("kind")
            target_name = target.get("name")
            target_source = target.get("src_path")
            if (
                not isinstance(kinds, list)
                or "test" not in kinds
                or target.get("test") is not True
                or not isinstance(target_name, str)
                or not isinstance(target_source, str)
            ):
                continue
            try:
                actual_source = Path(target_source).resolve(strict=True)
            except OSError:
                continue
            if actual_source == expected_source:
                matches.append(
                    {
                        "package_id": package_id,
                        "package_name": package_name,
                        "manifest_path": str(manifest_path),
                        "target_name": target_name,
                        "target_kind": list(kinds),
                        "src_path": str(actual_source),
                        "test": True,
                    }
                )
    return matches[0] if len(matches) == 1 else None

def _repository_relative_path(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty repository-relative path")
    candidate = Path(value)
    if candidate.is_absolute() or ".." in candidate.parts:
        raise ValueError(f"{label} must be repository-relative: {value!r}")
    return candidate.as_posix()


def load_expectations(path: Path | None, root: Path) -> tuple[dict[str, Any], dict[str, Any] | None]:
    if path is None:
        return {}, None
    raw = path.read_bytes()
    document = json.loads(raw.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_keys)
    schema_version = document.get("schema_version") if isinstance(document, dict) else None
    if (
        not isinstance(document, dict)
        or not isinstance(schema_version, int)
        or isinstance(schema_version, bool)
        or schema_version != EXPECTATION_SCHEMA_VERSION
    ):
        raise ValueError(
            f"expectations file must be a JSON object with schema_version: {EXPECTATION_SCHEMA_VERSION}"
        )
    inputs = document.get("inputs")
    if not isinstance(inputs, dict):
        raise ValueError("expectations file must contain an inputs object")
    expectations: dict[str, Any] = {}
    for key, value in inputs.items():
        if not isinstance(key, str) or not isinstance(value, dict):
            raise ValueError("each expectations entry must map a source path to an object")
        normalized = _repository_relative_path(key, label="expectation path")
        expectation_class = value.get("class")
        if not isinstance(expectation_class, str) or expectation_class not in EXPECTATION_CLASSES:
            raise ValueError(
                f"expectation {key!r} must have class in {sorted(EXPECTATION_CLASSES)}"
            )
        if normalized in expectations:
            raise ValueError(f"duplicate normalized expectation path: {normalized!r}")
        reason = value.get("reason")
        if expectation_class != "unmapped" and (
            not isinstance(reason, str) or not reason.strip()
        ):
            raise ValueError(f"expectation {key!r} must have a non-empty reason")
        if expectation_class == "unmapped" and reason is not None and not isinstance(reason, str):
            raise ValueError(f"expectation {key!r} reason must be a string when provided")

        input_sha256 = value.get("source_sha256")
        authority = value.get("authority")
        if expectation_class == "unmapped":
            if input_sha256 is not None and not _is_sha256(input_sha256):
                raise ValueError(f"expectation {key!r} source_sha256 must be a lowercase SHA-256")
            if authority is not None:
                raise ValueError(f"unmapped expectation {key!r} cannot claim test authority")
        else:
            if not _is_sha256(input_sha256):
                raise ValueError(
                    f"mapped expectation {key!r} must include source_sha256 for its Mimi input"
                )
            if not isinstance(authority, dict):
                raise ValueError(f"mapped expectation {key!r} must include an authority object")
            test_id = authority.get("test_id")
            assertion = authority.get("assertion")
            if not isinstance(test_id, str) or not test_id.strip():
                raise ValueError(f"mapped expectation {key!r} must include authority.test_id")
            if not isinstance(assertion, dict):
                raise ValueError(
                    f"mapped expectation {key!r} must include authority.assertion location and hash"
                )
            assertion_path = _repository_relative_path(
                assertion.get("path"), label=f"expectation {key!r} authority.assertion.path"
            )
            assertion_line = assertion.get("line")
            if not isinstance(assertion_line, int) or isinstance(assertion_line, bool) or assertion_line < 1:
                raise ValueError(f"expectation {key!r} authority.assertion.line must be a positive integer")
            assertion_sha256 = assertion.get("sha256")
            if not _is_sha256(assertion_sha256):
                raise ValueError(
                    f"expectation {key!r} authority.assertion.sha256 must be a lowercase SHA-256"
                )
            authority = {
                "test_id": test_id,
                "assertion": {
                    "path": assertion_path,
                    "line": assertion_line,
                    "sha256": assertion_sha256,
                },
            }

        expectation: dict[str, Any] = {
            "class": expectation_class,
            "reason": reason or ("expectation is not yet mapped" if expectation_class == "unmapped" else ""),
            "source_sha256": input_sha256,
            "authority": authority,
            "explicit": True,
        }
        if expectation_class in {"expected_build_rejection", "environment_failure"}:
            expected_exit_code = value.get("expected_exit_code")
            if (
                not isinstance(expected_exit_code, int)
                or isinstance(expected_exit_code, bool)
                or (
                    expectation_class == "expected_build_rejection"
                    and expected_exit_code <= 0
                )
                or (
                    expectation_class == "environment_failure"
                    and expected_exit_code == 0
                )
            ):
                raise ValueError(
                    f"expectation {key!r} must include a positive build-rejection exit code "
                    "or a nonzero environment-failure exit code"
                )
            expectation["expected_exit_code"] = expected_exit_code
        if expectation_class == "expected_build_rejection":
            expected_route_disposition = value.get("expected_route_disposition")
            if expected_route_disposition != "rejected":
                raise ValueError(
                    f"expectation {key!r} must declare expected_route_disposition: 'rejected'"
                )
            expected_route_status = value.get("expected_route_classification_status")
            if expected_route_status != "observed":
                raise ValueError(
                    f"expectation {key!r} must declare observed expected_route_classification_status"
                )
            expected_route_reason = value.get("expected_route_reason")
            if (
                not isinstance(expected_route_reason, str)
                or not expected_route_reason.strip()
                or expected_route_reason != expected_route_reason.strip()
                or "\n" in expected_route_reason
                or "\r" in expected_route_reason
            ):
                raise ValueError(
                    f"expectation {key!r} must include a trimmed single-line expected_route_reason"
                )
            expected_failure_class = value.get("expected_failure_class")
            if expected_failure_class not in {"capability", "library_target"}:
                raise ValueError(
                    f"expectation {key!r} must declare a recognized expected_failure_class"
                )
            diagnostic_codes = value.get("expected_diagnostic_codes")
            if (
                not isinstance(diagnostic_codes, list)
                or not diagnostic_codes
                or any(
                    not isinstance(code, str)
                    or not DIAGNOSTIC_CODE_FULL_RE.fullmatch(code)
                    for code in diagnostic_codes
                )
            ):
                raise ValueError(
                    f"expectation {key!r} must include non-empty expected_diagnostic_codes"
                )
            normalized_codes = [code.upper() for code in diagnostic_codes]
            if len(set(normalized_codes)) != len(normalized_codes):
                raise ValueError(f"expectation {key!r} has duplicate diagnostic codes")
            required_substrings = value.get("required_output_substrings")
            if (
                not isinstance(required_substrings, list)
                or not required_substrings
                or any(not isinstance(item, str) or not item for item in required_substrings)
            ):
                raise ValueError(
                    f"expectation {key!r} must include non-empty required_output_substrings"
                )
            expectation["expected_diagnostic_codes"] = sorted(normalized_codes)
            expectation["required_output_substrings"] = required_substrings
            expectation["expected_route_disposition"] = expected_route_disposition
            expectation["expected_route_classification_status"] = expected_route_status
            expectation["expected_route_reason"] = expected_route_reason
            expectation["expected_failure_class"] = expected_failure_class
        expectations[normalized] = expectation
    return expectations, {"path": str(path.resolve()), "sha256": sha256_bytes(raw), "bytes": len(raw)}


def bind_expectations_to_inventory(
    expectations: Mapping[str, Mapping[str, Any]],
    sources: Sequence[Mapping[str, Any]],
    root: Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Bind expectation claims to exact inputs and test-source assertions.

    This is a sidecar contract: it never changes the source inventory or build
    commands. Missing expectations become explicit ``unmapped`` rows and extra
    entries remain visible in the returned coverage summary.
    """
    inventory_paths = [str(item.get("path")) for item in sources]
    inventory_set = set(inventory_paths)
    duplicate_inventory_paths = sorted(
        name for name, count in Counter(inventory_paths).items() if count > 1
    )
    actor_contracts = actor_fixture_test_contracts(root)
    missing_paths = sorted(inventory_set - set(expectations))
    extra_paths = sorted(set(expectations) - inventory_set)
    bound: dict[str, Any] = {}

    for source in sources:
        name = str(source["path"])
        declared = expectations.get(name)
        if declared is None:
            bound[name] = {
                "class": "unmapped",
                "reason": "no expectation supplied",
                "source_sha256": source.get("sha256"),
                "authority": None,
                "explicit": False,
                "source_hash_status": "matched" if source.get("sha256") is None else "inventory_only",
                "authority_status": "not_applicable",
                "authority_valid": False,
            }
            continue

        item = dict(declared)
        item["path"] = name
        item["target_kind"] = source.get("target_kind", "unknown")
        item["authority_status"] = "not_applicable"
        item["authority_valid"] = False
        item["test_execution_valid"] = None
        if item.get("class") == "required_compile":
            item["test_contract_valid"] = False
            item["source_authority_valid"] = False
            item["cargo_target_valid"] = False
            item["cargo_test_target"] = None
            item["test_execution_valid"] = False
            item["test_execution_proof"] = None
        if item.get("class") == "expected_build_rejection":
            item["test_contract_valid"] = False
            item["source_authority_valid"] = False
            item["cargo_target_valid"] = False
            item["cargo_test_target"] = None
            item["test_execution_valid"] = False
            item["test_execution_proof"] = None
        if item.get("class") == "unmapped":
            input_hash_matches = None
            item["source_hash_status"] = "not_applicable"
        else:
            input_hash_matches = item.get("source_sha256") == source.get("sha256")
            item["source_hash_status"] = "matched" if input_hash_matches else "mismatch"
        authority = item.get("authority")
        if isinstance(authority, Mapping):
            assertion = authority.get("assertion")
            if isinstance(assertion, Mapping):
                assertion_path = str(assertion.get("path", ""))
                assertion_file = root / assertion_path
                try:
                    resolved = assertion_file.resolve(strict=True)
                    resolved.relative_to(root.resolve())
                    assertion_bytes = assertion_file.read_bytes()
                    assertion_text = assertion_bytes.decode("utf-8")
                    lines = assertion_text.splitlines()
                    assertion_hash_matches = sha256_bytes(assertion_bytes) == assertion.get("sha256")
                    test_id_present, assertion_line_valid = _test_authority_is_structurally_valid(
                        authority.get("test_id"),
                        assertion_text,
                        lines,
                        assertion.get("line"),
                    )
                    base_authority_valid = (
                        assertion_file.is_file()
                        and not assertion_file.is_symlink()
                        and assertion_hash_matches
                        and test_id_present
                        and assertion_line_valid
                    )
                    if item.get("class") == "expected_build_rejection":
                        item["test_contract_valid"] = (
                            base_authority_valid
                            and _expected_rejection_test_authority_is_valid(
                                test_id=authority.get("test_id"),
                                source_path=name,
                                target_kind=str(source.get("target_kind", "unknown")),
                                source_text=assertion_text,
                                assertion_line=assertion.get("line"),
                                expected_diagnostic_codes=item.get(
                                    "expected_diagnostic_codes"
                                ),
                                required_output_substrings=item.get(
                                    "required_output_substrings"
                                ),
                                expected_route_reason=item.get(
                                    "expected_route_reason"
                                ),
                            )
                        )
                        item["test_execution_valid"] = False
                        item["source_authority_valid"] = bool(
                            base_authority_valid and item["test_contract_valid"]
                        )
                    elif item.get("class") == "required_compile":
                        actor_contract = actor_contracts.get(name, {})
                        actor_authority = actor_contract.get("authority")
                        actor_specific_authority = bool(
                            name in ACTOR_FALLBACK_INPUTS
                            and actor_contract.get("authority_valid") is True
                            and isinstance(actor_authority, Mapping)
                            and dict(actor_authority) == dict(authority)
                        )
                        item["actor_specific_authority"] = actor_specific_authority
                        direct_test_contract = _required_compile_test_authority_is_valid(
                            test_id=authority.get("test_id"),
                            source_path=name,
                            target_kind=str(source.get("target_kind", "unknown")),
                            source_text=assertion_text,
                            assertion_line=assertion.get("line"),
                        )
                        item["test_contract_valid"] = (
                            actor_specific_authority or direct_test_contract
                        )
                        item["source_authority_valid"] = bool(
                            base_authority_valid and item["test_contract_valid"]
                        )
                        item["authority_valid"] = bool(
                            actor_specific_authority and item["source_authority_valid"]
                        )
                        item["authority_status"] = (
                            "actor_helper_authority"
                            if actor_specific_authority and item["source_authority_valid"]
                            else (
                                "requires_cargo_target_and_execution"
                                if item["source_authority_valid"]
                                else "required_compile_test_contract_mismatch"
                            )
                        )
                    regular_source_file = assertion_file.is_file() and not assertion_file.is_symlink()
                except (OSError, UnicodeDecodeError, ValueError, TypeError):
                    assertion_hash_matches = False
                    test_id_present = False
                    assertion_line_valid = False
                    regular_source_file = False
                    if item.get("class") == "expected_build_rejection":
                        item["test_contract_valid"] = False
                        item["test_execution_valid"] = False
                        item["source_authority_valid"] = False
                    elif item.get("class") == "required_compile":
                        item["test_contract_valid"] = False
                        item["test_execution_valid"] = False
                        item["source_authority_valid"] = False
                        item["authority_status"] = "required_compile_test_contract_mismatch"
                authority_valid = (
                    False
                    if item.get("class") in {"expected_build_rejection", "required_compile"}
                    else (
                        regular_source_file
                        and assertion_hash_matches
                        and test_id_present
                        and assertion_line_valid
                    )
                )
                item["authority_status"] = (
                    "requires_cargo_target_and_execution"
                    if item.get("class") == "expected_build_rejection"
                    and item.get("source_authority_valid") is True
                    else (
                        "rejection_test_contract_mismatch"
                        if item.get("class") == "expected_build_rejection"
                        else (
                            item.get("authority_status")
                            if item.get("class") == "required_compile"
                            else ("valid" if authority_valid else "source_location_or_hash_mismatch")
                        )
                    )
                )
                if item.get("class") != "required_compile":
                    item["authority_valid"] = authority_valid
        item["source_hash_matches"] = input_hash_matches
        bound[name] = item

    return bound, {
        "schema_version": EXPECTATION_SCHEMA_VERSION,
        "explicit_count": len(expectations),
        "mapped_count": sum(item.get("class") != "unmapped" for item in bound.values()),
        "missing_paths": missing_paths,
        "extra_paths": extra_paths,
        "duplicate_inventory_paths": duplicate_inventory_paths,
        "exact_bijection": not missing_paths and not extra_paths and not duplicate_inventory_paths,
        "invalid_source_hash_paths": sorted(
            name for name, item in bound.items() if item.get("source_hash_status") == "mismatch"
        ),
        "invalid_authority_paths": sorted(
            name
            for name, item in bound.items()
            if item.get("class") != "unmapped" and item.get("authority_valid") is not True
        ),
    }


def add_actor_default_expectations(
    expectations: Mapping[str, Mapping[str, Any]],
    sources: Sequence[Mapping[str, Any]],
    root: Path,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    """Bind the two required Actor inputs to their active default-route CLI tests.

    The defaults are generated from the checked-in fixture and test-source
    bytes at collection time, so the report pins both hashes and the assertion
    location. An explicitly supplied mapping always takes precedence.
    """
    result = {name: dict(value) for name, value in expectations.items()}
    by_path = {str(item.get("path")): item for item in sources}
    contracts = actor_fixture_test_contracts(root)
    defaults: list[dict[str, Any]] = []
    for source_path in ACTOR_FALLBACK_INPUTS:
        if source_path in result:
            continue
        source = by_path.get(source_path)
        contract = contracts.get(source_path, {})
        if (
            source is None
            or source.get("present") is not True
            or source.get("presence_kind") != "regular_file"
            or not _is_sha256(source.get("sha256"))
            or contract.get("authority_valid") is not True
        ):
            continue
        authority = contract["authority"]
        entry = {
            "class": "required_compile",
            "reason": (
                "required original Actor fixture must retain its source/output and use the "
                "default Canonical route without function fallback, then run its native output"
            ),
            "source_sha256": source["sha256"],
            "authority": authority,
            "expected_executable_stdout": contract["expected_stdout"],
            "explicit": True,
            "origin": "built_in_required_actor_fixture_contract",
        }
        result[source_path] = entry
        defaults.append({
            "path": source_path,
            "class": entry["class"],
            "origin": entry["origin"],
            "source_sha256": entry["source_sha256"],
            "authority": entry["authority"],
            "expected_executable_stdout": entry["expected_executable_stdout"],
        })
    return result, defaults


def _route_candidate_classification(
    route: Mapping[str, Any], expectation: Mapping[str, Any] | None
) -> str:
    """Assign one route class, requiring an exact contract for intentional rejection."""
    disposition = route.get("disposition")
    if disposition == "compatibility":
        return "compatibility"
    if disposition == "canonical":
        return "canonical"
    expectation_class = (
        expectation.get("class") if isinstance(expectation, Mapping) else None
    )
    if (
        disposition == "rejected"
        and expectation_class == "expected_build_rejection"
        and expectation is not None
        and expectation.get("matched") is True
    ):
        return "intentional_reject"
    if disposition in {None, "unobserved"}:
        if expectation_class in {None, "unmapped"}:
            return "unmapped"
    return "unknown"


def _inventory_only_record(
    source: Mapping[str, Any], index: int, expectations: Mapping[str, Mapping[str, Any]]
) -> dict[str, Any]:
    route = {"disposition": "unobserved", "classification_status": "not_run"}
    expectation = _expectation_result(
        str(source["path"]),
        None,
        route,
        expectations,
        run_status="not_run",
        captured_output=b"",
        output_artifact=None,
    )
    return {
        **dict(source),
        "input_index": index,
        "run_status": "not_run",
        "route": route,
        "expectation": expectation,
        "candidate_classification": _route_candidate_classification(route, expectation),
        "per_function": {"status": "unavailable", "named_roster_complete": False},
    }


def _route_observation(stdout: str, stderr: str, exit_code: int | None, run_status: str) -> dict[str, Any]:
    captured = stderr + "\n" + stdout
    diagnostic_text = _strip_ansi(captured)
    observations: list[dict[str, Any]] = []
    route_marker_count = 0
    for line in captured.splitlines():
        plain_line = _strip_ansi(line)
        route_matches = list(ROUTE_RE.finditer(plain_line))
        route_marker_count += len(route_matches)
        for marker_index, match in enumerate(route_matches):
            raw = match.group(1)
            observations.append({
                "disposition": "canonical" if raw == "canonical" else "compatibility",
                "raw_disposition": raw,
                "reason": match.group(2),
                "mir_digest": match.group(3),
                "line": line,
                "route_marker_index": marker_index,
            })
        rejected = REJECT_RE.search(plain_line) or EXPLICIT_REJECT_RE.search(plain_line)
        if rejected:
            observations.append({
                "disposition": "rejected",
                "raw_disposition": "rejected",
                "reason": rejected.group(1).strip() or "canonical route rejected",
                "mir_digest": None,
                "line": line,
            })
    if route_marker_count > 1:
        return {
            "disposition": "unobserved",
            "classification_status": "multiple_route_markers",
            "observations": observations,
            "route_marker_count": route_marker_count,
            "reason": "multiple canonical route markers were emitted; route selection is ambiguous",
            "mir_digest": None,
        }
    classes = {item["disposition"] for item in observations}
    route_identities = {
        (item["disposition"], item["reason"], item["mir_digest"])
        for item in observations
    }
    if len(route_identities) == 1:
        return {
            "disposition": next(iter(classes)),
            "classification_status": "observed",
            "observations": observations,
            "route_marker_count": route_marker_count,
            "reason": observations[0]["reason"] if len(observations) == 1 else None,
            "mir_digest": observations[0]["mir_digest"] if len(observations) == 1 else None,
        }
    if observations:
        return {
            "disposition": "unobserved",
            "classification_status": "conflicting_observations",
            "observations": observations,
            "route_marker_count": route_marker_count,
            "reason": "conflicting whole-program route identities or dispositions were emitted",
            "mir_digest": None,
        }
    if run_status not in {"completed"}:
        return {
            "disposition": "unobserved",
            "classification_status": run_status,
            "observations": [],
            "route_marker_count": 0,
            "reason": "route selection could not be observed because process did not complete",
            "mir_digest": None,
        }
    if exit_code not in (None, 0) and DIAGNOSTIC_RE.search(diagnostic_text):
        return {
            "disposition": "rejected",
            "classification_status": "inferred_pre_route_rejection",
            "observations": [],
            "route_marker_count": 0,
            "reason": "compiler emitted a failure diagnostic before any route telemetry",
            "mir_digest": None,
        }
    return {
        "disposition": "unobserved",
        "classification_status": "missing_route_telemetry",
        "observations": [],
        "route_marker_count": 0,
        "reason": "no whole-program route disposition was emitted",
        "mir_digest": None,
    }


def _diagnostic_codes(text: str) -> list[str]:
    """Return codes attached to recognized error-diagnostic lines only."""
    codes: set[str] = set()
    for raw_line in text.splitlines():
        match = DIAGNOSTIC_ERROR_CODE_RE.match(_strip_ansi(raw_line))
        if match:
            code = match.group("bracketed") or match.group("error_code") or match.group("bare")
            if code:
                codes.add(code.upper())
    return sorted(codes)


def _canonical_mir_function_observations(
    stdout: str,
    stderr: str,
    stats: Mapping[str, Any] | None,
    route: Mapping[str, Any],
) -> dict[str, Any]:
    phase_names = ("pre_route", "post_route")
    raw_events: list[dict[str, Any]] = []
    raw_summaries: list[dict[str, Any]] = []
    route_markers: list[dict[str, Any]] = []
    malformed_telemetry_lines: list[dict[str, Any]] = []

    # stdout and stderr are captured separately, so their relative ordering is
    # not evidence. A route marker can define phases only for telemetry from
    # that same stream.
    for stream_name, stream_text in (("stderr", stderr), ("stdout", stdout)):
        for line_index, line in enumerate(stream_text.splitlines()):
            match_line = _strip_ansi(line)
            route_matches = list(ROUTE_RE.finditer(match_line))
            for marker_index, route_match in enumerate(route_matches):
                route_markers.append({
                    "stream": stream_name,
                    "line_index": line_index,
                    "marker_index": marker_index,
                    "disposition": route_match.group(1),
                    "mir_digest": route_match.group(3),
                    "source_line": line,
                })

            event_prefixes = list(MIR_EMIT_SUCCESS_PREFIX_RE.finditer(match_line))
            summary_prefixes = list(MIR_EMIT_SUMMARY_PREFIX_RE.finditer(match_line))
            telemetry_prefix_count = len(event_prefixes) + len(summary_prefixes)
            if telemetry_prefix_count > 1:
                malformed_telemetry_lines.append({
                    "source_stream": stream_name,
                    "source_line_index": line_index,
                    "source_line": line,
                    "prefix_count": telemetry_prefix_count,
                    "event_prefix_count": len(event_prefixes),
                    "summary_prefix_count": len(summary_prefixes),
                    "reason": "multiple canonical MIR telemetry prefixes occur on one line",
                })
                continue
            if len(event_prefixes) == 1:
                match = MIR_EMIT_SUCCESS_RE.fullmatch(match_line)
                if match is None:
                    malformed_telemetry_lines.append({
                        "source_stream": stream_name,
                        "source_line_index": line_index,
                        "source_line": line,
                        "prefix_count": 1,
                        "event_prefix_count": 1,
                        "summary_prefix_count": 0,
                        "reason": "canonical MIR owner record is malformed or owner is not one structured token",
                    })
                    continue
                raw_events.append({
                    "function": match.group("owner"),
                    "owner": match.group("owner"),
                    "eligibility": "canonical_mir_owner",
                    "emission": "canonical_mir_native",
                    "fallback": False,
                    "status": "emission_success",
                    "source_stream": stream_name,
                    "source_line_index": line_index,
                    "source_line": line,
                })
                continue
            if len(summary_prefixes) == 1:
                match = MIR_EMIT_SUMMARY_RE.fullmatch(match_line)
                if match is None:
                    malformed_telemetry_lines.append({
                        "source_stream": stream_name,
                        "source_line_index": line_index,
                        "source_line": line,
                        "prefix_count": 1,
                        "event_prefix_count": 0,
                        "summary_prefix_count": 1,
                        "reason": "canonical MIR summary record is malformed",
                    })
                    continue
                raw_summaries.append({
                    "compiled": int(match.group(1)),
                    "total_mir_functions": int(match.group(2)),
                    "fell_back": int(match.group(3)),
                    "mir_digest": match.group(4),
                    "source_stream": stream_name,
                    "source_line_index": line_index,
                    "source_line": line,
                })

    route_boundary_unique = (
        len(route_markers) == 1 and route_markers[0]["disposition"] == "canonical"
    )
    route_boundary = route_markers[0] if route_boundary_unique else None

    def emission_phase(observation: Mapping[str, Any]) -> str:
        if route_boundary is None or observation["source_stream"] != route_boundary["stream"]:
            return "unclassified"
        return (
            "pre_route"
            if observation["source_line_index"] < route_boundary["line_index"]
            else "post_route"
        )

    events = [
        {**event, "emission_phase": emission_phase(event)}
        for event in raw_events
    ]
    summaries = [
        {**summary, "emission_phase": emission_phase(summary)}
        for summary in raw_summaries
    ]
    phase_events = {
        phase: [event for event in events if event["emission_phase"] == phase]
        for phase in phase_names
    }
    phase_summaries = {
        phase: [summary for summary in summaries if summary["emission_phase"] == phase]
        for phase in phase_names
    }
    observed_phases = [
        phase
        for phase in phase_names
        if phase_events[phase] or phase_summaries[phase]
    ]
    phase_rosters = {
        phase: [event["owner"] for event in phase_events[phase]]
        for phase in observed_phases
    }
    duplicate_owners_by_phase = {
        phase: sorted({
            owner
            for owner in phase_rosters[phase]
            if phase_rosters[phase].count(owner) > 1
        })
        for phase in observed_phases
    }
    duplicate_owners = sorted({
        owner
        for names in duplicate_owners_by_phase.values()
        for owner in names
    })
    phase_rosters_match = bool(observed_phases) and all(
        phase_rosters[phase] == phase_rosters[observed_phases[0]]
        for phase in observed_phases[1:]
    )

    per_phase_reconciliations: dict[str, dict[str, Any]] = {}
    for phase in observed_phases:
        phase_owner_events = phase_events[phase]
        phase_summary_rows = phase_summaries[phase]
        phase_summary = phase_summary_rows[0] if len(phase_summary_rows) == 1 else None
        per_phase_reconciliations[phase] = {
            "owner_roster": list(phase_rosters[phase]),
            "owner_event_count": len(phase_owner_events),
            "summary_count": len(phase_summary_rows),
            "summary": phase_summary,
            "owner_events_are_unique": not duplicate_owners_by_phase[phase],
            "named_owner_count_matches_total": (
                phase_summary is not None
                and len(phase_rosters[phase]) == phase_summary["total_mir_functions"]
            ),
            "emitter_counts_reconcile": (
                phase_summary is not None
                and phase_summary["compiled"] == len(phase_owner_events)
                and phase_summary["compiled"] + phase_summary["fell_back"]
                == phase_summary["total_mir_functions"]
            ),
            "graph_digest_matches_route": (
                phase_summary is not None
                and phase_summary["mir_digest"] == route.get("mir_digest")
            ),
            "route_has_no_fallback": (
                phase_summary is not None and phase_summary["fell_back"] == 0
            ),
        }

    all_telemetry_is_classified = (
        route_boundary_unique
        and all(event["emission_phase"] in phase_names for event in events)
        and all(summary["emission_phase"] in phase_names for summary in summaries)
    )
    phases_have_one_summary = bool(observed_phases) and all(
        len(phase_summaries[phase]) == 1 for phase in observed_phases
    )
    owner_events_unique_per_phase = bool(observed_phases) and all(
        not duplicate_owners_by_phase[phase] for phase in observed_phases
    )
    named_count_matches_total = bool(observed_phases) and all(
        per_phase_reconciliations[phase]["named_owner_count_matches_total"]
        for phase in observed_phases
    )
    summary_counts_reconcile = bool(observed_phases) and all(
        per_phase_reconciliations[phase]["emitter_counts_reconcile"]
        for phase in observed_phases
    )
    route_digest = route.get("mir_digest")
    digest_matches_route = (
        bool(summaries)
        and isinstance(route_digest, str)
        and bool(re.fullmatch(r"[0-9a-f]{64}", route_digest))
        and all(
            summary["mir_digest"] == route_digest
            for summary in summaries
        )
    )
    no_fallback = bool(summaries) and all(summary["fell_back"] == 0 for summary in summaries)
    stats_status = (
        stats.get("status")
        if isinstance(stats, Mapping) and "status" in stats and "parsed" in stats
        else ("available" if isinstance(stats, Mapping) else "not_emitted")
    )
    stats_payload = (
        stats.get("parsed")
        if isinstance(stats, Mapping) and "status" in stats and "parsed" in stats
        else stats
    )
    consistency: dict[str, bool | None] = {
        "mir_route_phase_boundary_is_unique": route_boundary_unique,
        "mir_emission_events_have_known_phases": all_telemetry_is_classified,
        "mir_telemetry_lines_are_well_formed": not malformed_telemetry_lines,
        "mir_summary_is_unique": phases_have_one_summary,
        "mir_owner_events_are_unique": owner_events_unique_per_phase,
        "mir_named_owner_count_matches_total": named_count_matches_total,
        "mir_emitter_counts_reconcile": summary_counts_reconcile,
        "mir_graph_digest_matches_route": digest_matches_route,
        "mir_route_has_no_fallback": no_fallback,
        "mir_phase_rosters_match": phase_rosters_match,
        "resolved_dispatch_stats_absent": stats_status == "not_emitted",
    }
    roster_complete = all(value is True for value in consistency.values())

    def consistent_summary_value(field: str) -> int | None:
        values = {summary[field] for summary in summaries}
        return next(iter(values)) if summaries and len(values) == 1 else None

    total = consistent_summary_value("total_mir_functions")
    compiled = consistent_summary_value("compiled")
    fell_back_values = [summary["fell_back"] for summary in summaries]
    fell_back = consistent_summary_value("fell_back")
    owners = [event["owner"] for event in events]
    unique_owner_count = len(set(owners))
    phase_missing_owner_counts = [
        max(0, phase_reconciliations["summary"]["total_mir_functions"] - len(set(phase_reconciliations["owner_roster"])))
        for phase_reconciliations in per_phase_reconciliations.values()
        if phase_reconciliations["summary"] is not None
    ]
    missing_owner_count = max(phase_missing_owner_counts) if phase_missing_owner_counts else None
    anonymous_slots = [
        {
            "slot": f"unattributed-mir-owner-{ordinal:06d}",
            "function": None,
            "identity_status": "mir_owner_not_emitted_by_compiler",
            "eligibility": "canonical_mir_owner",
            "emission": "unknown",
            "fallback": None,
        }
        for ordinal in range(1, (missing_owner_count or 0) + 1)
    ]
    fallback_count = max(fell_back_values, default=0)
    phase_success_counts = {
        phase: len(phase_events[phase]) for phase in observed_phases
    }
    phase_unique_owner_counts = {
        phase: len(set(phase_rosters[phase])) for phase in observed_phases
    }
    phase_fallback_counts = {
        phase: per_phase_reconciliations[phase]["summary"]["fell_back"]
        if per_phase_reconciliations[phase]["summary"] is not None
        else None
        for phase in observed_phases
    }
    cross_phase_repeated_owners = sorted(
        set.intersection(*(
            set(phase_rosters[phase]) for phase in observed_phases
        ))
    ) if len(observed_phases) > 1 else []
    return {
        "status": "complete_named" if roster_complete else (
            "partial" if events or summaries or stats_status != "not_emitted" else "unavailable"
        ),
        "source": "canonical MIR native owner events and summaries partitioned at the unique canonical route disposition",
        "dispatch_stats": dict(stats_payload) if isinstance(stats_payload, Mapping) else None,
        "dispatch_stats_status": stats_status,
        "counts": {
            "total_functions": total,
            "compiled_canonical_mir": compiled,
            "canonical_mir_fallback": fell_back,
            "named_emission_successes": len(events),
            "unique_named_functions": unique_owner_count,
            "unattributed_function_count": missing_owner_count,
            "fallback_total_observed_or_counted": fallback_count,
            "emission_phase_count": len(observed_phases),
            "named_emission_successes_by_phase": phase_success_counts,
            "unique_named_functions_by_phase": phase_unique_owner_counts,
            "canonical_mir_fallback_by_phase": phase_fallback_counts,
        },
        "functions": events,
        "unattributed_function_slots": anonymous_slots,
        "emitter_summary": summaries[0] if len(summaries) == 1 else None,
        "emitter_summaries": summaries,
        "malformed_telemetry_lines": malformed_telemetry_lines,
        "emission_phase_rosters": phase_rosters,
        "emission_phase_reconciliations": per_phase_reconciliations,
        "route_markers": route_markers,
        "route_marker_count": len(route_markers),
        "route_mir_digest": route_digest,
        "consistency": consistency,
        "duplicate_function_names": duplicate_owners,
        "duplicate_function_names_by_phase": duplicate_owners_by_phase,
        "cross_phase_repeated_owners": cross_phase_repeated_owners,
        "named_roster_complete": roster_complete,
        "has_fallback": fallback_count > 0 or no_fallback is False,
        "limitation": None if roster_complete else (
            "Canonical MIR owner events or summaries are absent, unclassified, duplicated within "
            "an emission phase, malformed or duplicated on a source line, or inconsistent across "
            "phases with the MIR graph count/digest or route identity; resolved dispatch stats do "
            "not describe this emitter. --check fails closed."
        ),
    }


def _function_observations(
    stdout: str,
    stderr: str,
    stats: Mapping[str, Any] | None,
    route: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    route_observations = route.get("observations", []) if isinstance(route, Mapping) else []
    has_canonical_route_marker = (
        isinstance(route, Mapping)
        and (
            route.get("disposition") == "canonical"
            or any(
                isinstance(observation, Mapping)
                and observation.get("raw_disposition") == "canonical"
                for observation in route_observations
            )
        )
    )
    if has_canonical_route_marker:
        return _canonical_mir_function_observations(stdout, stderr, stats, route)

    events: list[dict[str, Any]] = []
    summaries: list[dict[str, int]] = []
    for line in (stderr + "\n" + stdout).splitlines():
        match_line = _strip_ansi(line)
        match = SKIP_RE.match(match_line)
        if match:
            events.append({
                "function": match.group(1),
                "eligibility": "ineligible",
                "emission": "legacy_fallback",
                "fallback": True,
                "status": "eligibility_skip",
                "skip_reason": match.group(2),
                "source_line": line,
            })
            continue
        match = EMIT_FALLBACK_RE.match(match_line)
        if match:
            events.append({
                "function": match.group(1),
                "eligibility": "eligible",
                "emission": "legacy_fallback",
                "fallback": True,
                "status": "emission_fallback",
                "skip_reason": None,
                "emit_failure_reason": match.group(2),
                "source_line": line,
            })
            continue
        match = EMIT_VERIFY_FAIL_RE.match(match_line)
        if match:
            events.append({
                "function": match.group(1),
                "eligibility": "eligible",
                "emission": "legacy_fallback",
                "fallback": True,
                "status": "emission_verification_failed",
                "skip_reason": None,
                "emit_failure_reason": "LLVM function verification failed (detail follows in raw stderr)",
                "source_line": line,
            })
            continue
        match = EMIT_SUCCESS_RE.match(match_line)
        if match:
            events.append({
                "function": match.group(1),
                "eligibility": "eligible",
                "emission": "resolved",
                "fallback": False,
                "status": "emission_success",
                "skip_reason": None,
                "source_line": line,
            })
            continue
        match = EMIT_SUMMARY_RE.match(match_line)
        if match:
            summaries.append({
                "compiled": int(match.group(1)),
                "attempted_eligible": int(match.group(2)),
                "fell_back_to_legacy": int(match.group(3)),
            })
    if isinstance(stats, Mapping) and "status" in stats and "parsed" in stats:
        parsed_stats = stats.get("parsed")
    else:
        parsed_stats = stats
    parsed_stats = dict(parsed_stats) if isinstance(parsed_stats, Mapping) else None
    skip_histogram = parsed_stats.get("skip_reasons") if parsed_stats else None
    summary = summaries[0] if len(summaries) == 1 else None
    counters = {
        field: parsed_stats.get(field) if parsed_stats else None
        for field in ("total_functions", "eligible", "legacy_fallback", "emit_failed")
    }
    for field, value in counters.items():
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            counters[field] = None

    skip_events = [event for event in events if event["status"] == "eligibility_skip"]
    emit_fallback_events = [
        event for event in events
        if event["status"] in {"emission_fallback", "emission_verification_failed"}
    ]
    emit_success_events = [event for event in events if event["status"] == "emission_success"]
    names = [event["function"] for event in events]
    duplicate_names = sorted({name for name in names if names.count(name) > 1})
    total = counters["total_functions"]
    eligible = counters["eligible"]
    skipped = counters["legacy_fallback"]
    summary_compiled = summary["compiled"] if summary else None
    summary_fallback = summary["fell_back_to_legacy"] if summary else None
    observed_named = len(set(names))
    unattributed_count = max(0, total - observed_named) if total is not None else None

    consistency: dict[str, bool | None] = {
        "dispatch_total_reconciles": (
            total == eligible + skipped if total is not None and eligible is not None and skipped is not None else None
        ),
        "named_eligibility_skips_reconcile": (
            len(skip_events) == skipped if skipped is not None else None
        ),
        "emitter_summary_is_unique": len(summaries) == 1 if eligible not in (None, 0) else len(summaries) <= 1,
        "emitter_summary_reconciles_eligibility": (
            summary["attempted_eligible"] == eligible if summary and eligible is not None else (True if eligible == 0 else None)
        ),
        "named_emission_fallbacks_reconcile": (
            len(emit_fallback_events) == summary_fallback if summary_fallback is not None else (True if eligible == 0 else None)
        ),
        "named_emission_successes_reconcile": (
            len(emit_success_events) == summary_compiled if summary_compiled is not None else (True if eligible == 0 else None)
        ),
        "function_names_are_unique": not duplicate_names,
        "all_function_names_are_observed": (
            total == observed_named if total is not None else None
        ),
    }
    reconciled = all(value is True for value in consistency.values())
    # Preserve unattributed aggregate slots explicitly. These are not fabricated
    # function identities; --check rejects them as incomplete evidence.
    anonymous_slots: list[dict[str, Any]] = []
    if unattributed_count is not None:
        for ordinal in range(1, unattributed_count + 1):
            anonymous_slots.append({
                "slot": f"unattributed-function-{ordinal:06d}",
                "function": None,
                "identity_status": "name_not_emitted_by_compiler",
                "eligibility": "eligible" if eligible is not None and summary_compiled is not None else "unknown",
                "emission": "resolved" if summary_compiled is not None else "unknown",
                "fallback": None,
            })
    roster_complete = reconciled and unattributed_count == 0
    fallback_count = (skipped or 0) + len(emit_fallback_events)
    return {
        "status": "complete_named" if roster_complete else (
            "partial" if parsed_stats is not None or events else "unavailable"
        ),
        "source": "MIMI_STAT JSON plus MIMI_VERBOSE per-function events when present",
        "dispatch_stats": parsed_stats,
        "counts": {
            **counters,
            "compiled_resolved": summary_compiled,
            "emission_fallback_from_verbose_summary": summary_fallback,
            "named_ineligible": len(skip_events),
            "named_emission_fallbacks": len(emit_fallback_events),
            "named_emission_successes": len(emit_success_events),
            "unique_named_functions": observed_named,
            "unattributed_function_count": unattributed_count,
            "fallback_total_observed_or_counted": fallback_count,
        },
        "skip_reason_histogram": skip_histogram if isinstance(skip_histogram, dict) else None,
        "functions": events,
        "unattributed_function_slots": anonymous_slots,
        "emitter_summary": summary,
        "consistency": consistency,
        "duplicate_function_names": duplicate_names,
        "named_roster_complete": roster_complete,
        "has_fallback": fallback_count > 0,
        "limitation": None if roster_complete else (
            "Named eligibility and emitter events are absent, incomplete, duplicated, or inconsistent "
            "with aggregate counts. Unnamed slots remain explicit and make the named roster incomplete; "
            "--check fails closed. Expected success event format: info: resolved emitter emitted "
            "'<qualified-name>'."
        ),
    }


def _load_dispatch_stats(directory: Path) -> dict[str, Any]:
    files = sorted(path for path in directory.glob("*.json") if path.is_file())
    entries: list[dict[str, Any]] = []
    for path in files:
        try:
            raw = path.read_bytes()
        except OSError as error:
            entries.append({
                "file": path.name,
                "parse_status": "unreadable",
                "parse_error": str(error),
            })
            continue
        item: dict[str, Any] = {
            "file": path.name,
            "sha256": sha256_bytes(raw),
            "bytes": len(raw),
            "raw_text": _decode(raw),
        }
        try:
            item["parsed"] = json.loads(raw.decode("utf-8"))
            item["parse_status"] = "valid_json"
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            item["parse_status"] = "invalid_json"
            item["parse_error"] = str(error)
        entries.append(item)
    valid = [entry["parsed"] for entry in entries if entry.get("parse_status") == "valid_json"]
    if not files:
        status = "not_emitted"
    elif len(files) != 1:
        status = "ambiguous"
    elif entries[0].get("parse_status") != "valid_json" or not isinstance(entries[0].get("parsed"), dict):
        status = "invalid"
    else:
        status = "available"
    return {"status": status, "files": entries, "parsed": valid[0] if status == "available" else None}


def _first_failure_match(
    text: str, patterns: Sequence[re.Pattern[str]]
) -> tuple[str, str] | None:
    for pattern in patterns:
        match = pattern.search(text)
        if match:
            return pattern.pattern, match.group(0)
    return None


def _failure_classification(
    *, run_status: str, exit_code: int | None, text: str, target_kind: str, route: Mapping[str, Any]
) -> dict[str, Any]:
    if run_status in {"timeout", "spawn_error"}:
        return {
            "class": "environment",
            "status": "classified",
            "reason": "Mimi process timed out or could not be started",
            "evidence": {"run_status": run_status},
        }
    if run_status == "completed" and exit_code == 0:
        return {"class": "none", "status": "not_applicable", "reason": "process exited successfully", "evidence": None}
    if run_status != "completed" or exit_code is None:
        return {
            "class": "unknown",
            "status": "unclassified",
            "reason": f"process did not produce a classifiable completed build ({run_status})",
            "evidence": {"run_status": run_status, "exit_code": exit_code},
        }

    diagnostic_text = _strip_ansi(text)
    environment_match = _first_failure_match(diagnostic_text, ENVIRONMENT_FAILURE_PATTERNS)
    if environment_match or TOOLCHAIN_FAILURE_RE.search(diagnostic_text):
        pattern, matched = environment_match or (
            TOOLCHAIN_FAILURE_RE.pattern,
            TOOLCHAIN_FAILURE_RE.search(diagnostic_text).group(0),
        )
        return {
            "class": "environment",
            "status": "classified",
            "reason": "recognized host/toolchain execution failure",
            "evidence": {"pattern": pattern, "matched_text": matched},
        }

    external_link_pattern = next(
        (pattern for pattern in EXTERNAL_LINK_FAILURE_PATTERNS if pattern.search(diagnostic_text)),
        None,
    )
    if external_link_pattern is not None:
        external_link_match = external_link_pattern.search(diagnostic_text)
        assert external_link_match is not None
        linker_line = next(
            (
                line
                for line in text.splitlines()
                if external_link_pattern.search(_strip_ansi(line))
            ),
            external_link_match.group(0),
        )
        return {
            "class": "external_dependency",
            "status": "classified",
            "reason": "linker reported an unresolved external dependency",
            "evidence": {
                "pattern": external_link_pattern.pattern,
                "matched_text": external_link_match.group(0),
                "symbol": external_link_match.group("symbol"),
                "linker_line": linker_line,
            },
        }

    library_match = _first_failure_match(diagnostic_text, LIBRARY_TARGET_FAILURE_PATTERNS)
    if library_match:
        return {
            "class": "library_target",
            "status": "classified",
            "reason": "diagnostic identifies the requested shared-library target",
            "evidence": {"pattern": library_match[0], "matched_text": library_match[1]},
        }

    capability_match = _first_failure_match(diagnostic_text, CAPABILITY_FAILURE_PATTERNS)
    if capability_match or (
        route.get("disposition") == "rejected"
        and route.get("classification_status") == "observed"
    ):
        evidence = (
            {"pattern": capability_match[0], "matched_text": capability_match[1]}
            if capability_match
            else {"route_rejection": route.get("reason")}
        )
        return {
            "class": "capability",
            "status": "classified",
            "reason": "compiler reported an explicit Canonical/native capability rejection",
            "evidence": evidence,
        }

    if (
        route.get("disposition") == "rejected"
        and route.get("classification_status") == "inferred_pre_route_rejection"
    ):
        source_diagnostics = _source_diagnostic_lines(text)
        if source_diagnostics:
            return {
                "class": "source_diagnostic",
                "status": "classified",
                "reason": "compiler rejected a Mimi source input before route selection",
                "evidence": {
                    "diagnostic_count": len(source_diagnostics),
                    "first_diagnostic": source_diagnostics[0],
                    "diagnostics": source_diagnostics,
                },
            }
    return {
        "class": "unknown",
        "status": "unclassified",
        "reason": "nonzero exit has no recognized capability, library-target, or environment evidence",
        "evidence": {"exit_code": exit_code, "target_kind": target_kind},
    }


def _expectation_result(
    source_path: str,
    exit_code: int | None,
    route: Mapping[str, Any],
    expectations: Mapping[str, Mapping[str, Any]],
    *,
    run_status: str = "completed",
    failure_classification: Mapping[str, Any] | None = None,
    captured_output: bytes | str = b"",
    output_artifact: Mapping[str, Any] | None = None,
    executable_run: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    expectation = expectations.get(source_path)
    if expectation is None:
        expectation = {
            "class": "unmapped",
            "reason": "no expectation supplied",
            "source_sha256": None,
            "authority": None,
            "explicit": False,
            "source_hash_status": "missing",
            "authority_status": "not_applicable",
            "authority_valid": False,
        }
    expectation_class = str(expectation.get("class", "unmapped"))
    failure = failure_classification if isinstance(failure_classification, Mapping) else {}
    valid_evidence = (
        expectation_class == "unmapped"
        or (
            expectation.get("explicit") is True
            and expectation.get("source_hash_status") == "matched"
            and expectation.get("authority_valid") is True
            and (
                (
                    expectation_class == "required_compile"
                    and expectation.get("actor_specific_authority") is True
                )
                or expectation_class not in {"required_compile", "expected_build_rejection"}
                or (
                    expectation.get("test_execution_valid") is True
                    and isinstance(expectation.get("test_execution_proof"), Mapping)
                    and expectation["test_execution_proof"].get("status") == "passed"
                )
            )
        )
    )
    decoded_output = _decode(captured_output)
    diagnostic_output = _strip_ansi(decoded_output)
    actual_diagnostic_codes = _diagnostic_codes(decoded_output)
    outcome_match = False
    expected_outcome: dict[str, Any]
    diagnostic_contract: dict[str, Any] | None = None
    route_contract: dict[str, Any] | None = None
    executable_contract: dict[str, Any] | None = None
    if expectation_class == "required_compile":
        expected_outcome = {
            "build": "completed",
            "exit_code": 0,
            "route": {
                "disposition": "canonical",
                "classification_status": "observed",
            },
        }
        outcome_match = (
            run_status == "completed"
            and exit_code == 0
            and route.get("disposition") == "canonical"
            and route.get("classification_status") == "observed"
        )
        expected_executable_stdout = expectation.get("expected_executable_stdout")
        if expected_executable_stdout is not None:
            expected_native_bytes = _captured_bytes(expected_executable_stdout)
            native_run = executable_run if isinstance(executable_run, Mapping) else {}
            actual_native_bytes = _captured_bytes(native_run.get("stdout_raw"))
            native_stdout_matches = (
                expected_native_bytes is not None
                and actual_native_bytes is not None
                and actual_native_bytes == expected_native_bytes
            )
            native_exit_code_matches = (
                native_run.get("run_status") == "completed"
                and native_run.get("exit_code") == 0
            )
            executable_contract = {
                "valid": expected_native_bytes is not None,
                "expected_stdout": expected_executable_stdout,
                "actual_stdout": native_run.get("stdout_raw"),
                "stdout_matches_exactly": native_stdout_matches,
                "run_status": native_run.get("run_status"),
                "actual_exit_code": native_run.get("exit_code"),
                "exit_code_matches": native_exit_code_matches,
                "matches": native_stdout_matches and native_exit_code_matches,
            }
            expected_outcome["native_executable"] = {
                "exit_code": 0,
                "stdout": expected_executable_stdout,
            }
            outcome_match = outcome_match and executable_contract["matches"]
    elif expectation_class == "expected_build_rejection":
        expected_code = expectation.get("expected_exit_code")
        expected_exit_code_valid = (
            isinstance(expected_code, int)
            and not isinstance(expected_code, bool)
            and expected_code > 0
        )
        raw_expected_codes = expectation.get("expected_diagnostic_codes")
        expected_codes_valid = (
            isinstance(raw_expected_codes, list)
            and bool(raw_expected_codes)
            and all(
                isinstance(code, str) and DIAGNOSTIC_CODE_FULL_RE.fullmatch(code)
                for code in raw_expected_codes
            )
        )
        expected_diagnostic_codes = (
            sorted({code.upper() for code in raw_expected_codes})
            if expected_codes_valid
            else []
        )
        raw_substrings = expectation.get("required_output_substrings")
        substrings_valid = (
            isinstance(raw_substrings, list)
            and bool(raw_substrings)
            and all(isinstance(item, str) and item for item in raw_substrings)
        )
        required_substrings = raw_substrings if substrings_valid else []
        expected_route_disposition = expectation.get("expected_route_disposition")
        expected_route_status = expectation.get("expected_route_classification_status")
        expected_route_reason = expectation.get("expected_route_reason")
        expected_failure_class = expectation.get("expected_failure_class")
        route_contract_valid = (
            expected_route_disposition == "rejected"
            and expected_route_status == "observed"
            and isinstance(expected_route_reason, str)
            and bool(expected_route_reason.strip())
            and expected_failure_class in {"capability", "library_target"}
        )
        route_contract = {
            "valid": route_contract_valid,
            "expected_disposition": expected_route_disposition,
            "actual_disposition": route.get("disposition"),
            "disposition_matches": (
                route_contract_valid
                and route.get("disposition") == expected_route_disposition
            ),
            "expected_classification_status": expected_route_status,
            "actual_classification_status": route.get("classification_status"),
            "classification_status_matches": (
                route_contract_valid
                and route.get("classification_status") == expected_route_status
            ),
            "expected_reason": expected_route_reason,
            "actual_reason": route.get("reason"),
            "reason_matches_exactly": (
                route_contract_valid
                and route.get("reason") == expected_route_reason
            ),
            "expected_failure_class": expected_failure_class,
            "actual_failure_class": failure.get("class"),
            "failure_class_matches": (
                route_contract_valid
                and failure.get("class") == expected_failure_class
            ),
        }
        missing_substrings = [
            item for item in required_substrings if item not in diagnostic_output
        ]
        diagnostic_codes_match = (
            expected_codes_valid and actual_diagnostic_codes == expected_diagnostic_codes
        )
        required_substrings_match = substrings_valid and not missing_substrings
        no_output_artifact = output_artifact is None
        diagnostic_contract = {
            "valid": expected_exit_code_valid and expected_codes_valid and substrings_valid,
            "expected_exit_code_valid": expected_exit_code_valid,
            "expected_diagnostic_codes": expected_diagnostic_codes,
            "actual_diagnostic_codes": actual_diagnostic_codes,
            "diagnostic_codes_match_exactly": diagnostic_codes_match,
            "required_output_substrings": required_substrings,
            "missing_output_substrings": missing_substrings,
            "required_output_substrings_match": required_substrings_match,
            "output_artifact_absent": no_output_artifact,
        }
        expected_outcome = {
            "build": "completed",
            "exit_code": expected_code,
            "route": {
                "disposition": expected_route_disposition,
                "classification_status": expected_route_status,
                "reason": expected_route_reason,
            },
            "failure_class": expected_failure_class,
            "diagnostic_contract": diagnostic_contract,
            "output_artifact": "absent",
        }
        outcome_match = (
            run_status == "completed"
            and expected_exit_code_valid
            and exit_code == expected_code
            and route_contract_valid
            and route.get("disposition") == expected_route_disposition
            and route.get("classification_status") == expected_route_status
            and route.get("reason") == expected_route_reason
            and failure.get("class") == expected_failure_class
            and failure.get("status") == "classified"
            and diagnostic_codes_match
            and required_substrings_match
            and no_output_artifact
        )
    elif expectation_class == "environment_failure":
        expected_code = expectation.get("expected_exit_code")
        expected_outcome = {
            "build": "completed",
            "exit_code": expected_code,
            "failure_class": "environment",
        }
        outcome_match = (
            run_status == "completed"
            and exit_code == expected_code
            and failure.get("class") == "environment"
        )
    elif expectation_class == "interpreter_only":
        expected_outcome = {
            "build": "not_run",
            "reason": "interpreter-only input",
            "output_artifact": "absent",
        }
        outcome_match = (
            run_status == "not_run"
            and exit_code is None
            and output_artifact is None
        )
    elif expectation_class == "known_gap":
        expected_outcome = {"build": "known_gap_unresolved"}
    else:
        expected_outcome = {"build": "unmapped"}

    matched = valid_evidence and outcome_match
    if expectation_class == "unmapped":
        status = "unmapped"
    elif expectation_class == "known_gap":
        status = "known_gap_unresolved"
    elif expectation_class == "interpreter_only" and run_status != "not_run":
        status = "build_was_run"
    elif not valid_evidence:
        status = "invalid_evidence"
    else:
        status = "matched" if matched else "mismatch"
    return {
        "class": expectation_class,
        "status": status,
        "explicit_expectation": bool(expectation.get("explicit")),
        "reason": expectation.get("reason"),
        "expected": expected_outcome,
        "source_sha256": expectation.get("source_sha256"),
        "source_hash_status": expectation.get("source_hash_status"),
        "source_hash_matches": expectation.get("source_hash_matches"),
        "authority": expectation.get("authority"),
        "authority_status": expectation.get("authority_status"),
        "authority_valid": expectation.get("authority_valid"),
        "test_contract_valid": expectation.get("test_contract_valid"),
        "source_authority_valid": expectation.get("source_authority_valid"),
        "actor_specific_authority": expectation.get("actor_specific_authority"),
        "target_kind": expectation.get("target_kind"),
        "cargo_target_valid": expectation.get("cargo_target_valid"),
        "cargo_test_target": expectation.get("cargo_test_target"),
        "test_execution_valid": expectation.get("test_execution_valid"),
        "test_execution_proof": expectation.get("test_execution_proof"),
        "path": source_path,
        "run_status": run_status,
        "actual_exit_code": exit_code,
        "actual_route": route.get("disposition"),
        "actual_route_classification_status": route.get("classification_status"),
        "actual_route_reason": route.get("reason"),
        "actual_failure_class": failure.get("class"),
        "actual_failure_reason": failure.get("reason"),
        "actual_diagnostic_codes": actual_diagnostic_codes,
        "diagnostic_contract": diagnostic_contract,
        "route_contract": route_contract,
        "executable_contract": executable_contract,
        "actual_output_artifact_present": output_artifact is not None,
        "outcome_matches": outcome_match,
        "evidence_valid": valid_evidence,
        "matched": matched,
    }


def _actor_fallback_evidence(
    manifest: Sequence[Mapping[str, Any]],
    records: Sequence[Mapping[str, Any]],
    actor_contracts: Mapping[str, Mapping[str, Any]] | None = None,
    actor_test_execution: Mapping[str, Any] | None = None,
    root: Path = ROOT,
    cargo_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    by_path = {str(record.get("path")): record for record in records}
    manifest_by_path = {str(record.get("path")): record for record in manifest}
    manifest_paths = set(manifest_by_path)
    execution_cases = (
        actor_test_execution.get("cases", [])
        if isinstance(actor_test_execution, Mapping)
        else []
    )
    execution_by_path = {
        str(item.get("path")): item
        for item in execution_cases
        if isinstance(item, Mapping)
    }
    cases: list[dict[str, Any]] = []
    for path in ACTOR_FALLBACK_INPUTS:
        record = by_path.get(path)
        source = manifest_by_path.get(path)
        contract = (actor_contracts or {}).get(path, {})
        rust_test_execution = execution_by_path.get(path)
        executable_run = record.get("native_executable_run") if record else None
        expected_stdout = contract.get("expected_stdout") or _raw_capture(
            ACTOR_EXPECTED_STDOUT[path]
        )
        executable_stdout = (
            _captured_bytes(executable_run.get("stdout_raw"))
            if isinstance(executable_run, Mapping)
            else None
        )
        expected_stdout_bytes = _captured_bytes(expected_stdout)
        native_stdout_matches = (
            executable_stdout is not None
            and expected_stdout_bytes is not None
            and executable_stdout == expected_stdout_bytes
        )
        native_exit_matches = (
            isinstance(executable_run, Mapping)
            and executable_run.get("run_status") == "completed"
            and executable_run.get("exit_code") == 0
        )
        cases.append({
            "path": path,
            "in_denominator": path in manifest_paths,
            "present": bool(source and source.get("present")),
            "tracked_by_git": bool(source and source.get("tracked_by_git")),
            "run_status": record.get("run_status") if record else "not_run",
            "exit_code": record.get("exit_code") if record else None,
            "whole_program_disposition": record.get("route", {}).get("disposition") if record else "not_run",
            "whole_program_route_reason": record.get("route", {}).get("reason") if record else None,
            "per_function": record.get("per_function") if record else None,
            "native_executable_run": executable_run,
            "native_test_authority": contract.get("authority"),
            "native_test_authority_valid": contract.get("authority_valid") is True,
            "native_test_helper_name": contract.get("helper_name"),
            "native_test_helper_sha256": contract.get("helper_sha256"),
            "native_test_helper_sha256_expected": contract.get("helper_sha256_expected"),
            "native_test_helper_valid": contract.get("helper_valid") is True,
            "rust_test_execution": rust_test_execution,
            "rust_test_execution_valid": (
                _actor_test_execution_valid(
                    rust_test_execution,
                    path,
                    source,
                    contract,
                    root=root,
                    cargo_authority=cargo_authority,
                )
                if rust_test_execution is not None
                else False
            ),
            "expected_native_stdout": expected_stdout,
            "native_stdout_matches_exactly": native_stdout_matches,
            "native_exit_code_matches": native_exit_matches,
            "native_executable_contract_matches": (
                native_stdout_matches and native_exit_matches
            ),
            "failure_classification": record.get("failure_classification") if record else None,
            "evidence_complete": bool(
                record
                and isinstance(record.get("route"), Mapping)
                and isinstance(record.get("per_function"), Mapping)
                and isinstance(record.get("stdout_raw"), Mapping)
                and isinstance(record.get("stderr_raw"), Mapping)
                and isinstance(executable_run, Mapping)
                and isinstance(executable_run.get("stdout_raw"), Mapping)
                and isinstance(executable_run.get("stderr_raw"), Mapping)
            ),
            "raw_build_logs": {
                "stdout": record.get("stdout_raw") if record else None,
                "stderr": record.get("stderr_raw") if record else None,
            },
        })
    missing = [
        case["path"]
        for case in cases
        if not case["in_denominator"] or not case["present"] or not case["evidence_complete"]
    ]
    return {
        "required_cases": list(ACTOR_FALLBACK_INPUTS),
        "cases": cases,
        "rust_test_execution": actor_test_execution,
        "missing_cases": missing,
        "source": "B-01/A-05 explicitly retain both former nested capture/shadow compatibility cases in the route denominator",
    }


def _parse_legacy_owner_audit(stdout: str) -> dict[str, Any]:
    owners: dict[str, dict[str, Any]] = {}
    owner_field_counts: Counter[tuple[str, str]] = Counter()
    duplicate_owner_fields: list[str] = []
    unknown_owner_names: set[str] = set()
    malformed_condition_rows: list[str] = []
    malformed_owner_rows: list[str] = []
    pending_condition_owner: str | None = None
    retired_summaries: list[re.Match[str]] = []
    retired_summary_count = 0

    def assign_owner_field(name: str, field: str, value: Any) -> None:
        if name not in ACTIVE_LEGACY_OWNER_NAMES:
            unknown_owner_names.add(name)
            return
        owner_field_counts[(name, field)] += 1
        if owner_field_counts[(name, field)] > 1:
            duplicate_owner_fields.append(f"{name}.{field}")
            return
        owners.setdefault(name, {})[field] = value

    retirement_pattern = re.compile(
        r"retired_flow_dual_owners=physical;ast-owner=deleted;file-adapter=deleted;"
        r"transition-vir-adapter=deleted;MIR-positive-negative-single-dual-evidence="
        r"([^+]+)\+([^+]+)\+([^+]+)$"
    )
    for line in stdout.splitlines():
        owner_prefix = re.match(r"owner=([^ ]+)", line)
        if owner_prefix and owner_prefix.group(1) not in LEGACY_OWNER_NAMES:
            unknown_owner_names.add(owner_prefix.group(1))
        if line.startswith("retired_flow_dual_owners="):
            retired_summary_count += 1
            match = retirement_pattern.fullmatch(line)
            if match is not None:
                retired_summaries.append(match)
            continue
        match = re.match(r"owner=([^ ]+) production_accessor_call_sites=(\d+)$", line)
        if match:
            assign_owner_field(
                match.group(1), "production_accessor_call_sites", int(match.group(2))
            )
            continue
        match = re.match(
            r"owner=([^ ]+) production_access_status=([^ ]+) dependency_class=([^ ]+) "
            r"owner_deletion_ready=([01]) owner_deletion_blocker=(.*)",
            line,
        )
        if match:
            name = match.group(1)
            assign_owner_field(name, "production_access_status", match.group(2))
            assign_owner_field(name, "dependency_class", match.group(3))
            assign_owner_field(name, "owner_deletion_ready", match.group(4) == "1")
            assign_owner_field(name, "owner_deletion_blocker", match.group(5))
            continue
        match = re.match(r"owner=([^ ]+) owner_deletion_condition_digest=([0-9a-f]{64})$", line)
        if match:
            if pending_condition_owner is not None:
                malformed_condition_rows.append("condition digest before prior condition text")
            name = match.group(1)
            assign_owner_field(name, "deletion_condition_sha256", match.group(2))
            pending_condition_owner = name
            continue
        match = re.match(r"owner_deletion_condition=(.*)$", line)
        if match:
            if pending_condition_owner is None:
                malformed_condition_rows.append("condition text without a preceding owner digest")
            else:
                assign_owner_field(
                    pending_condition_owner, "deletion_condition", match.group(1)
                )
                pending_condition_owner = None
            continue
        if owner_prefix:
            name = owner_prefix.group(1)
            is_known_context_row = (
                name in ACTIVE_LEGACY_OWNER_NAMES
                and (
                    re.fullmatch(
                        r"owner=[^ ]+ evidence_scope=.+ evidence_test=\S+ evidence_marker=.+",
                        line,
                    )
                    is not None
                    or re.fullmatch(
                        r"owner=[^ ]+ closed_route_guard=\S+ context=.+",
                        line,
                    )
                    is not None
                )
            )
            if name in LEGACY_OWNER_NAMES and not is_known_context_row:
                malformed_owner_rows.append(line)
    if pending_condition_owner is not None:
        malformed_condition_rows.append("owner digest without condition text")
    parsed = [
        {"name": name, **owners.get(name, {})}
        for name in ACTIVE_LEGACY_OWNER_NAMES
    ]
    actual_condition_set = "".join(
        f"{owner['name']}={owner.get('deletion_condition_sha256', '')}\n"
        for owner in parsed
    )
    condition_set_values = re.findall(
        r"(?m)^owner_deletion_condition_set_digest=([^\r\n]*)$", stdout
    )
    reported_set_digest = condition_set_values[0] if len(condition_set_values) == 1 else None
    condition_digests_match = all(
        owner.get("deletion_condition_sha256") == sha256_bytes(
            str(owner.get("deletion_condition", "")).encode("utf-8")
        )
        for owner in parsed
    )
    condition_set_digest_matches = (
        reported_set_digest == sha256_bytes(actual_condition_set.encode("utf-8"))
    )
    owner_rows_complete = all(
        isinstance(owner.get("production_accessor_call_sites"), int)
        and owner.get("production_access_status") in {"sealed", "open"}
        and isinstance(owner.get("dependency_class"), str)
        and isinstance(owner.get("owner_deletion_ready"), bool)
        and isinstance(owner.get("owner_deletion_blocker"), str)
        and isinstance(owner.get("deletion_condition"), str)
        and bool(owner.get("deletion_condition"))
        and isinstance(owner.get("deletion_condition_sha256"), str)
        for owner in parsed
    )
    owner_count_values = re.findall(r"(?m)^owner_count=(\d+)$", stdout)
    owner_count_reported = int(owner_count_values[0]) if len(owner_count_values) == 1 else None
    dynamic_probe_values = re.findall(r"(?m)^dynamic_probe_status=([^\r\n]*)$", stdout)
    dynamic_probe_status = dynamic_probe_values[0] if len(dynamic_probe_values) == 1 else None
    audit_status_values = re.findall(r"(?m)^audit_status=([^\r\n]*)$", stdout)
    audit_status = audit_status_values[0] if len(audit_status_values) == 1 else None
    retired_test_names = (
        list(retired_summaries[0].groups()) if len(retired_summaries) == 1 else []
    )
    retired_owner_evidence_complete = (
        retired_summary_count == 1
        and len(retired_summaries) == 1
        and tuple(retired_test_names) == RETIRED_FLOW_DUAL_PARITY_TESTS
        and dynamic_probe_status == "not_run"
    )
    retired_owners = [
        {
            "name": name,
            "status": "physically_retired" if retired_owner_evidence_complete else "unverified",
            "retirement_evidence_kind": "audit_script_source_checks",
            "retirement_evidence_complete": retired_owner_evidence_complete,
            "declared_parity_test_names": retired_test_names,
            "dynamic_probe_status": dynamic_probe_status,
        }
        for name in RETIRED_LEGACY_OWNER_NAMES
    ]
    owner_set_complete = (
        {owner["name"] for owner in parsed} == set(ACTIVE_LEGACY_OWNER_NAMES)
        and len(parsed) == len(ACTIVE_LEGACY_OWNER_NAMES)
        and owner_count_reported == len(ACTIVE_LEGACY_OWNER_NAMES)
        and not unknown_owner_names
        and not duplicate_owner_fields
        and not malformed_condition_rows
        and not malformed_owner_rows
        and len(condition_set_values) == 1
        and len(owner_count_values) == 1
        and audit_status == "ok"
        and len(audit_status_values) == 1
    )
    complete = (
        owner_set_complete
        and owner_rows_complete
        and condition_digests_match
        and condition_set_digest_matches
        and retired_owner_evidence_complete
    )
    return {
        "owners": parsed,
        "retired_owners": retired_owners,
        "owner_count_reported": owner_count_reported,
        "owner_deletion_condition_set_sha256": reported_set_digest,
        "deletion_condition_digests_match_text": condition_digests_match,
        "deletion_condition_set_digest_matches": condition_set_digest_matches,
        "retired_owner_evidence_complete": retired_owner_evidence_complete,
        "retired_owner_evidence_status": "complete" if retired_owner_evidence_complete else "incomplete",
        "retired_owner_parity_tests_declared": retired_test_names,
        "dynamic_probe_status": dynamic_probe_status,
        "unknown_owner_names": sorted(unknown_owner_names),
        "duplicate_owner_fields": duplicate_owner_fields,
        "malformed_condition_rows": malformed_condition_rows,
        "malformed_owner_rows": malformed_owner_rows,
        "parsed_owner_set_complete": complete,
    }


def _parse_assignment(text: str, key: str) -> str | None:
    match = re.search(rf"(?m)^{re.escape(key)}=([^\r\n]*)$", text)
    return match.group(1) if match else None


def _parse_int_assignment(text: str, key: str) -> int | None:
    value = _parse_assignment(text, key)
    return int(value) if value is not None and value.isdigit() else None


def _collect_legacy_owner_audit(
    root: Path, logs_root: Path, index: int = 0
) -> dict[str, Any]:
    script = root / "scripts" / "audit-mir-legacy-owners.sh"
    command = ["bash", str(script)]
    started = time.time()
    stdout_bytes = b""
    stderr_bytes = b""
    exit_code: int | None = None
    run_status = "completed"
    if not script.is_file():
        run_status = "audit_script_missing"
        stderr_bytes = f"required legacy-owner audit script missing: {script}\n".encode()
    else:
        try:
            proc = subprocess.run(
                command,
                cwd=root,
                capture_output=True,
                timeout=120,
                check=False,
            )
            stdout_bytes = proc.stdout or b""
            stderr_bytes = proc.stderr or b""
            exit_code = proc.returncode
        except subprocess.TimeoutExpired as error:
            run_status = "timeout"
            stdout_bytes = error.output or b""
            stderr_bytes = error.stderr or b""
        except OSError as error:
            run_status = "spawn_error"
            stderr_bytes = str(error).encode("utf-8", errors="replace")
    stdout_text = _decode(stdout_bytes)
    stderr_text = _decode(stderr_bytes)
    stdout_artifact = _write_raw_artifact(logs_root / "legacy-owner-audit.stdout.bin", stdout_bytes)
    stderr_artifact = _write_raw_artifact(logs_root / "legacy-owner-audit.stderr.bin", stderr_bytes)
    parsed = _parse_legacy_owner_audit(stdout_text)
    expected_active = set(ACTIVE_LEGACY_OWNER_NAMES)
    expected_retired = set(RETIRED_LEGACY_OWNER_NAMES)
    actual_active = {
        str(owner["name"])
        for owner in parsed["owners"]
        if owner.get("production_accessor_call_sites") is not None
    }
    actual_retired = {
        str(owner["name"])
        for owner in parsed["retired_owners"]
        if owner.get("retirement_evidence_complete") is True
    }
    accepted = (
        run_status == "completed"
        and exit_code == 0
        and parsed["owner_count_reported"] == len(ACTIVE_LEGACY_OWNER_NAMES)
        and len(parsed["owners"]) == len(ACTIVE_LEGACY_OWNER_NAMES)
        and actual_active == expected_active
        and len(parsed["retired_owners"]) == len(RETIRED_LEGACY_OWNER_NAMES)
        and actual_retired == expected_retired
        and parsed["retired_owner_evidence_complete"]
        and parsed["parsed_owner_set_complete"]
        and bool(re.fullmatch(r"[0-9a-f]{64}", parsed["owner_deletion_condition_set_sha256"] or ""))
        and _parse_assignment(stdout_text, "audit_status") == "ok"
    )
    return {
        "command": command,
        "run_status": run_status,
        "exit_code": exit_code,
        "elapsed_seconds": round(time.time() - started, 6),
        "script_sha256": sha256_file(script) if script.is_file() else None,
        "stdout": stdout_text,
        "stderr": stderr_text,
        "stdout_raw": stdout_artifact,
        "stderr_raw": stderr_artifact,
        **parsed,
        "accepted_as_complete_inventory": accepted,
        "failure_status": "none" if accepted else "owner_audit_incomplete_or_failed",
    }


def _write_raw_artifact(path: Path, data: bytes) -> dict[str, Any]:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    return {"path": path.name, "sha256": sha256_bytes(data), "bytes": len(data), "base64": base64.b64encode(data).decode("ascii")}


def _output_artifact_observation(path: Path) -> dict[str, Any] | None:
    """Report every filesystem object at the output path; only regular files are hashed."""
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as error:
        return {"status": "unreadable", "error": str(error)}
    if not stat.S_ISREG(metadata.st_mode):
        if stat.S_ISDIR(metadata.st_mode):
            kind = "directory"
        elif stat.S_ISLNK(metadata.st_mode):
            kind = "symlink"
        elif stat.S_ISFIFO(metadata.st_mode):
            kind = "fifo"
        elif stat.S_ISSOCK(metadata.st_mode):
            kind = "socket"
        elif stat.S_ISCHR(metadata.st_mode):
            kind = "character_device"
        elif stat.S_ISBLK(metadata.st_mode):
            kind = "block_device"
        else:
            kind = "other"
        return {
            "status": "non_regular_path",
            "kind": kind,
            "mode": stat.S_IFMT(metadata.st_mode),
            "bytes": metadata.st_size,
        }

    try:
        return {"sha256": sha256_file(path), "bytes": metadata.st_size}
    except OSError as error:
        return {"status": "unreadable", "error": str(error)}


def _raw_sidecar_matches(logs_root: Path | None, capture: Any) -> bool:
    """Require a regular non-symlink sidecar whose bytes equal its inline capture."""
    if logs_root is None or not isinstance(capture, Mapping):
        return False
    name = capture.get("path")
    expected = _captured_bytes(capture)
    if (
        not isinstance(name, str)
        or not name
        or Path(name).is_absolute()
        or Path(name).name != name
        or Path(name).suffix != ".bin"
        or expected is None
    ):
        return False

    # The artifact directory is itself part of the trust boundary. Reject a
    # symlink at any component instead of allowing a caller-provided artifact
    # root to redirect the evidence check outside its permitted location.
    root_path = Path(os.path.abspath(logs_root))
    cursor = Path(root_path.anchor)
    try:
        for component in root_path.parts[1:]:
            cursor /= component
            metadata = cursor.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                return False
        root_metadata = root_path.lstat()
        if not stat.S_ISDIR(root_metadata.st_mode):
            return False

        directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        nofollow = getattr(os, "O_NOFOLLOW", None)
        if nofollow is None:
            return False
        root_descriptor = os.open(root_path, directory_flags | nofollow)
        try:
            opened_root = os.fstat(root_descriptor)
            if (
                not stat.S_ISDIR(opened_root.st_mode)
                or (opened_root.st_dev, opened_root.st_ino)
                != (root_metadata.st_dev, root_metadata.st_ino)
            ):
                return False

            path = root_path / name
            before = path.lstat()
            if not stat.S_ISREG(before.st_mode):
                return False
            descriptor = os.open(name, os.O_RDONLY | nofollow, dir_fd=root_descriptor)
            try:
                opened = os.fstat(descriptor)
                if (
                    not stat.S_ISREG(opened.st_mode)
                    or (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
                ):
                    return False
                with os.fdopen(descriptor, "rb", closefd=False) as stream:
                    actual = stream.read()
            finally:
                os.close(descriptor)
        finally:
            os.close(root_descriptor)
    except OSError:
        return False
    return actual == expected


def _cargo_exact_test_output(stdout: bytes, test_id: str) -> bool:
    return _actor_test_pass_output(stdout, test_id)


def _run_cargo_authority_command(
    command: list[str],
    root: Path,
    timeout: float,
    cargo_authority: Mapping[str, Any],
) -> dict[str, Any]:
    cargo_identity = cargo_authority.get("cargo", {})
    rustc_identity = cargo_authority.get("rustc", {})
    cargo_before = _executable_observation(cargo_identity.get("path"))
    rustc_before = _executable_observation(rustc_identity.get("path"))
    rustup_identity = cargo_authority.get("rustup", {})
    rustup_before = _executable_observation(rustup_identity.get("path"))
    rustup_toolchain_expected = cargo_authority.get("rustup_toolchain", {})
    current_authority_before = cargo_test_toolchain_authority(
        root,
        probe_active_toolchain=False,
        expected_active_toolchain=rustup_toolchain_expected,
    )
    rustup_toolchain_before = current_authority_before.get("rustup_toolchain", {})
    run_status = "completed"
    exit_code: int | None
    stdout = b""
    stderr = b""
    authority_before_valid = (
        bool(command)
        and command[0] == cargo_identity.get("path")
        and _cargo_toolchain_identity_valid(
            cargo_authority,
            root=root,
            require_current_environment=True,
            current_authority=current_authority_before,
        )
        and cargo_before == {
            "path": cargo_identity.get("path"),
            "resolved_path": cargo_identity.get("resolved_path"),
            "sha256": cargo_identity.get("sha256"),
        }
        and rustc_before == {
            "path": rustc_identity.get("path"),
            "resolved_path": rustc_identity.get("resolved_path"),
            "sha256": rustc_identity.get("sha256"),
        }
        and rustup_before == {
            "path": rustup_identity.get("path"),
            "resolved_path": rustup_identity.get("resolved_path"),
            "sha256": rustup_identity.get("sha256"),
        }
    )
    if not authority_before_valid:
        run_status = "cargo_toolchain_authority_invalid_before_run"
        exit_code = None
        stderr = b"Cargo/rustc identity, environment, or Cargo config failed the frozen authority policy\n"
    else:
        child_environment = os.environ.copy()
        # Cargo always invokes the already-identified compiler. An inherited
        # RUSTC/RUSTC_WRAPPER/target runner is rejected before reaching here.
        child_environment["RUSTC"] = str(rustc_identity["path"])
        child_environment.pop("RUSTC_WRAPPER", None)
        child_environment.pop("RUSTC_WORKSPACE_WRAPPER", None)
        child_environment.pop("RUSTFLAGS", None)
        child_environment.pop("CARGO_ENCODED_RUSTFLAGS", None)
        for name in list(child_environment):
            if re.fullmatch(r"CARGO_TARGET_.+_(?:RUNNER|LINKER|RUSTFLAGS)", name):
                child_environment.pop(name, None)
        try:
            process = subprocess.run(
                command,
                cwd=root,
                capture_output=True,
                timeout=timeout,
                check=False,
                env=child_environment,
            )
            exit_code = process.returncode
            stdout = process.stdout or b""
            stderr = process.stderr or b""
        except subprocess.TimeoutExpired as error:
            run_status = "timeout"
            exit_code = None
            stdout = error.output or b""
            stderr = error.stderr or b""
        except OSError as error:
            run_status = "spawn_error"
            exit_code = None
            stderr = str(error).encode("utf-8", errors="replace")
    cargo_after = _executable_observation(cargo_identity.get("path"))
    rustc_after = _executable_observation(rustc_identity.get("path"))
    rustup_after = _executable_observation(rustup_identity.get("path"))
    current_authority_after = cargo_test_toolchain_authority(
        root,
        probe_active_toolchain=False,
        expected_active_toolchain=rustup_toolchain_expected,
    )
    rustup_toolchain_after = current_authority_after.get("rustup_toolchain", {})
    authority_after_valid = _cargo_toolchain_identity_valid(
        cargo_authority,
        root=root,
        require_current_environment=True,
        current_authority=current_authority_after,
    )
    stable = bool(
        authority_before_valid
        and authority_after_valid
        and cargo_after == cargo_before
        and rustc_after == rustc_before
        and rustup_after == rustup_before
        and rustup_toolchain_before == rustup_toolchain_expected
        and rustup_toolchain_after == rustup_toolchain_expected
    )
    cargo_execution = _cargo_execution_evidence(
        cargo_authority,
        cargo_before,
        rustc_before,
        cargo_after,
        rustc_after,
        stable=stable,
        rustup_before=rustup_before,
        rustup_after=rustup_after,
        rustup_toolchain_before=rustup_toolchain_before,
        rustup_toolchain_after=rustup_toolchain_after,
    )
    return {
        "command": command,
        "command_sha256": sha256_bytes(
            json.dumps(command, separators=(",", ":")).encode("utf-8")
        ),
        "cwd": str(root),
        "run_status": run_status,
        "exit_code": exit_code,
        "stdout_bytes": stdout,
        "stderr_bytes": stderr,
        "cargo_execution": cargo_execution,
    }


def _authority_command_record(
    result: Mapping[str, Any], logs_root: Path, stem: str
) -> dict[str, Any]:
    stdout = result.get("stdout_bytes")
    stderr = result.get("stderr_bytes")
    stdout_bytes = stdout if isinstance(stdout, bytes) else b""
    stderr_bytes = stderr if isinstance(stderr, bytes) else b""
    return {
        "command": list(result.get("command", [])),
        "command_sha256": result.get("command_sha256"),
        "cargo_execution": result.get("cargo_execution"),
        "cwd": result.get("cwd"),
        "run_status": result.get("run_status"),
        "exit_code": result.get("exit_code"),
        "stdout_raw": _write_raw_artifact(
            logs_root / f"{stem}.stdout.bin", stdout_bytes
        ),
        "stderr_raw": _write_raw_artifact(
            logs_root / f"{stem}.stderr.bin", stderr_bytes
        ),
    }


def _expected_rejection_execution_proof_is_valid(
    expectation: Mapping[str, Any],
    *,
    root: Path,
    logs_root: Path | None,
    cargo_authority: Mapping[str, Any] | None = None,
) -> bool:
    """Recheck captured Cargo target, compile, exact test and source evidence."""
    proof = expectation.get("test_execution_proof")
    authority = expectation.get("authority")
    assertion = authority.get("assertion") if isinstance(authority, Mapping) else None
    target = proof.get("cargo_test_target") if isinstance(proof, Mapping) else None
    metadata_evidence = proof.get("cargo_metadata") if isinstance(proof, Mapping) else None
    build = proof.get("target_build") if isinstance(proof, Mapping) else None
    run = proof.get("exact_test_run") if isinstance(proof, Mapping) else None
    if not all(
        isinstance(value, Mapping)
        for value in (assertion, target, metadata_evidence, build, run)
    ):
        return False
    test_id = authority.get("test_id")
    authority_path = assertion.get("path")
    expected_authority_hash = assertion.get("sha256")
    fixture_path = expectation.get("path")
    expected_fixture_hash = expectation.get("source_sha256")
    if (
        not isinstance(test_id, str)
        or not isinstance(authority_path, str)
        or not isinstance(fixture_path, str)
        or not _is_sha256(expected_authority_hash)
        or not _is_sha256(expected_fixture_hash)
        or proof.get("status") not in {"pending", "passed"}
        or proof.get("test_id") != test_id
        or proof.get("authority_path") != authority_path
        or proof.get("fixture_path") != fixture_path
        or proof.get("cargo_toolchain_identity_sha256")
        != (cargo_authority.get("identity_sha256") if isinstance(cargo_authority, Mapping) else None)
    ):
        return False

    metadata_stdout = _captured_bytes(metadata_evidence.get("stdout_raw"))
    metadata_stderr = _captured_bytes(metadata_evidence.get("stderr_raw"))
    cargo_path = (
        cargo_authority.get("cargo", {}).get("path")
        if isinstance(cargo_authority, Mapping)
        else None
    )
    metadata_command = [cargo_path, "metadata", "--format-version", "1", "--no-deps"]
    if (
        metadata_evidence.get("command") != metadata_command
        or not _cargo_execution_evidence_valid(
            metadata_evidence.get("cargo_execution"), cargo_authority, root=root
        )
        or metadata_evidence.get("cwd") != str(root)
        or metadata_evidence.get("run_status") != "completed"
        or metadata_evidence.get("exit_code") != 0
        or metadata_stdout is None
        or metadata_stderr is None
    ):
        return False
    try:
        metadata = json.loads(metadata_stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return False
    selected_target = _cargo_test_target_for_source(metadata, root, authority_path)
    if selected_target is None or dict(target) != selected_target:
        return False

    manifest_path = str(target.get("manifest_path"))
    target_name = str(target.get("target_name"))
    expected_build_command = [
        cargo_path, "test", "--manifest-path", manifest_path,
        "--test", target_name, "--no-run",
    ]
    expected_run_command = [
        cargo_path, "test", "--manifest-path", manifest_path,
        "--test", target_name, test_id, "--", "--exact", "--nocapture",
    ]
    if (
        build.get("command") != expected_build_command
        or run.get("command") != expected_run_command
        or build.get("cwd") != str(root)
        or run.get("cwd") != str(root)
        or build.get("run_status") != "completed"
        or build.get("exit_code") != 0
        or run.get("run_status") != "completed"
        or run.get("exit_code") != 0
    ):
        return False

    for command_record in (build, run):
        command = command_record.get("command")
        expected_command_hash = (
            sha256_bytes(json.dumps(command, separators=(",", ":")).encode("utf-8"))
            if isinstance(command, list)
            else None
        )
        if (
            command_record.get("command_sha256") != expected_command_hash
            or not _cargo_execution_evidence_valid(
                command_record.get("cargo_execution"), cargo_authority, root=root
            )
            or _captured_bytes(command_record.get("stdout_raw")) is None
            or _captured_bytes(command_record.get("stderr_raw")) is None
            or not _raw_sidecar_matches(logs_root, command_record.get("stdout_raw"))
            or not _raw_sidecar_matches(logs_root, command_record.get("stderr_raw"))
        ):
            return False
    if not _cargo_exact_test_output(
        _captured_bytes(run.get("stdout_raw")) or b"", test_id
    ):
        return False

    authority_hashes = proof.get("authority_source_hashes")
    fixture_hashes = proof.get("fixture_source_hashes")
    return bool(
        isinstance(authority_hashes, Mapping)
        and authority_hashes.get("expected") == expected_authority_hash
        and authority_hashes.get("before_build") == expected_authority_hash
        and authority_hashes.get("after_build") == expected_authority_hash
        and authority_hashes.get("before_test") == expected_authority_hash
        and authority_hashes.get("after_test") == expected_authority_hash
        and isinstance(fixture_hashes, Mapping)
        and fixture_hashes.get("path") == fixture_path
        and fixture_hashes.get("expected") == expected_fixture_hash
        and fixture_hashes.get("before_build") == expected_fixture_hash
        and fixture_hashes.get("after_build") == expected_fixture_hash
        and fixture_hashes.get("before_test") == expected_fixture_hash
        and fixture_hashes.get("after_test") == expected_fixture_hash
    )


def _required_compile_source_authority_is_valid(
    expectation: Mapping[str, Any], *, root: Path
) -> bool:
    """Recheck a schema-v4 required-compile source contract from current bytes."""
    if expectation.get("class") != "required_compile":
        return False
    if expectation.get("actor_specific_authority") is True:
        return True
    authority = expectation.get("authority")
    assertion = authority.get("assertion") if isinstance(authority, Mapping) else None
    source_path = expectation.get("path")
    if (
        not isinstance(authority, Mapping)
        or not isinstance(assertion, Mapping)
        or not isinstance(source_path, str)
        or not isinstance(assertion.get("path"), str)
        or not _is_sha256(assertion.get("sha256"))
    ):
        return False
    assertion_file = root / str(assertion["path"])
    try:
        resolved = assertion_file.resolve(strict=True)
        resolved.relative_to(root.resolve(strict=True))
        if not assertion_file.is_file() or assertion_file.is_symlink():
            return False
        raw = assertion_file.read_bytes()
        text = raw.decode("utf-8")
    except (OSError, UnicodeDecodeError, ValueError):
        return False
    lines = text.splitlines()
    test_id_present, assertion_line_valid = _test_authority_is_structurally_valid(
        authority.get("test_id"), text, lines, assertion.get("line")
    )
    return bool(
        sha256_bytes(raw) == assertion.get("sha256")
        and test_id_present
        and assertion_line_valid
        and _required_compile_test_authority_is_valid(
            test_id=authority.get("test_id"),
            source_path=source_path,
            target_kind=str(expectation.get("target_kind", "unknown")),
            source_text=text,
            assertion_line=assertion.get("line"),
        )
    )


def _required_compile_execution_proof_is_valid(
    expectation: Mapping[str, Any],
    *,
    root: Path,
    logs_root: Path | None,
    cargo_authority: Mapping[str, Any] | None = None,
) -> bool:
    """Require the exact active Cargo test to pass with stable source hashes."""
    if (
        expectation.get("class") != "required_compile"
        or expectation.get("test_contract_valid") is not True
        or expectation.get("source_authority_valid") is not True
        or not _required_compile_source_authority_is_valid(expectation, root=root)
    ):
        return False
    return _expected_rejection_execution_proof_is_valid(
        expectation,
        root=root,
        logs_root=logs_root,
        cargo_authority=cargo_authority,
    )


def _run_required_compile_authority_tests(
    root: Path,
    logs_root: Path,
    sources: Sequence[Mapping[str, Any]],
    expectations: Mapping[str, dict[str, Any]],
    timeout: float,
    cargo_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Compile and run exact Rust authorities for generic required_compile rows.

    The two built-in Actor authorities use their independently pinned helper
    contract and are intentionally left to ``_run_actor_authority_tests``.
    """
    source_by_path = {str(source.get("path")): source for source in sources}
    required_items = {
        path: item
        for path, item in expectations.items()
        if isinstance(item, dict)
        and item.get("class") == "required_compile"
        and item.get("actor_specific_authority") is not True
    }
    if not required_items:
        return {"status": "not_required", "cargo_metadata": None, "targets": [], "tests": []}
    if cargo_authority is None:
        cargo_authority = cargo_test_toolchain_authority(root)

    metadata_result, metadata_document = _cargo_metadata(root, timeout, cargo_authority)
    metadata_record = {
        "command": metadata_result["command"],
        "cargo_toolchain_identity_sha256": metadata_result["cargo_toolchain_identity_sha256"],
        "cargo_execution": metadata_result["cargo_execution"],
        "cwd": metadata_result["cwd"],
        "run_status": metadata_result["run_status"],
        "exit_code": metadata_result["exit_code"],
        "stdout_raw": metadata_result["stdout_raw"],
        "stderr_raw": metadata_result["stderr_raw"],
    }
    groups: dict[tuple[str, str, str], list[tuple[str, dict[str, Any], dict[str, Any]]]] = {}
    rejected: list[str] = []
    for path, item in sorted(required_items.items()):
        item["cargo_metadata"] = metadata_record
        item["cargo_target_valid"] = False
        item["cargo_test_target"] = None
        item["authority_valid"] = False
        item["test_execution_valid"] = False
        source = source_by_path.get(path)
        authority = item.get("authority")
        assertion = authority.get("assertion") if isinstance(authority, Mapping) else None
        if (
            source is None
            or item.get("source_authority_valid") is not True
            or item.get("test_contract_valid") is not True
            or not isinstance(authority, Mapping)
            or not isinstance(assertion, Mapping)
            or metadata_result.get("run_status") != "completed"
            or metadata_result.get("exit_code") != 0
        ):
            item["authority_status"] = "cargo_target_or_source_contract_unproven"
            rejected.append(path)
            continue
        test_id = authority.get("test_id")
        authority_path = assertion.get("path")
        target = _cargo_test_target_for_source(metadata_document, root, str(authority_path))
        if not isinstance(test_id, str) or target is None:
            item["authority_status"] = "not_a_cargo_test_target"
            rejected.append(path)
            continue
        item["cargo_test_target"] = target
        item["cargo_target_valid"] = True
        item["authority_status"] = "valid_cargo_test_target"
        key = (str(target["manifest_path"]), str(target["target_name"]), str(target["package_id"]))
        groups.setdefault(key, []).append((path, item, target))

    target_reports: list[dict[str, Any]] = []
    test_reports: list[dict[str, Any]] = []
    for target_index, (key, entries) in enumerate(sorted(groups.items()), 1):
        manifest_path, target_name, _package_id = key
        fixture_before = {
            path: _regular_file_sha256(root / path) for path, _item, _target in entries
        }
        authority_paths = {
            str(item["authority"]["assertion"]["path"])
            for _path, item, _target in entries
        }
        authority_before = {
            path: _regular_file_sha256(root / path) for path in authority_paths
        }
        cargo_path = str(cargo_authority.get("cargo", {}).get("path"))
        build_command = [
            cargo_path, "test", "--manifest-path", manifest_path,
            "--test", target_name, "--no-run",
        ]
        build_result = _run_cargo_authority_command(
            build_command, root, timeout, cargo_authority
        )
        fixture_after_build = {
            path: _regular_file_sha256(root / path) for path, _item, _target in entries
        }
        authority_after_build = {
            path: _regular_file_sha256(root / path) for path in authority_paths
        }
        target_stem = sha256_bytes("\0".join(key).encode("utf-8"))[:12]
        build_record = _authority_command_record(
            build_result, logs_root,
            f"required-compile-target-{target_index:03d}-{target_stem}.build",
        )
        target_reports.append({
            "target": entries[0][2],
            "build": build_record,
            "fixture_sha256_before": fixture_before,
            "fixture_sha256_after": fixture_after_build,
            "authority_sha256_before": authority_before,
            "authority_sha256_after": authority_after_build,
        })
        for test_index, (path, item, target) in enumerate(entries, 1):
            authority = item["authority"]
            assertion = authority["assertion"]
            test_id = str(authority["test_id"])
            authority_path = str(assertion["path"])
            run_command = [
                cargo_path, "test", "--manifest-path", manifest_path,
                "--test", target_name, test_id, "--", "--exact", "--nocapture",
            ]
            source_before_test = _regular_file_sha256(root / authority_path)
            fixture_before_test = _regular_file_sha256(root / path)
            if build_record.get("run_status") == "completed" and build_record.get("exit_code") == 0:
                run_result = _run_cargo_authority_command(
                    run_command, root, timeout, cargo_authority
                )
                run_record = _authority_command_record(
                    run_result,
                    logs_root,
                    f"required-compile-target-{target_index:03d}-{target_stem}.test-{test_index:03d}",
                )
            else:
                run_record = {
                    "command": run_command,
                    "command_sha256": sha256_bytes(
                        json.dumps(run_command, separators=(",", ":")).encode("utf-8")
                    ),
                    "cwd": str(root),
                    "run_status": "not_run_target_build_failed",
                    "exit_code": None,
                    "stdout_raw": None,
                    "stderr_raw": None,
                }
            source_after_test = _regular_file_sha256(root / authority_path)
            fixture_after_test = _regular_file_sha256(root / path)
            proof = {
                "status": "pending",
                "cargo_toolchain_identity_sha256": cargo_authority.get("identity_sha256"),
                "test_id": test_id,
                "authority_path": authority_path,
                "fixture_path": path,
                "cargo_metadata": metadata_record,
                "cargo_test_target": target,
                "target_build": build_record,
                "exact_test_run": run_record,
                "authority_source_hashes": {
                    "expected": assertion.get("sha256"),
                    "before_build": authority_before.get(authority_path),
                    "after_build": authority_after_build.get(authority_path),
                    "before_test": source_before_test,
                    "after_test": source_after_test,
                },
                "fixture_source_hashes": {
                    "path": path,
                    "expected": item.get("source_sha256"),
                    "before_build": fixture_before.get(path),
                    "after_build": fixture_after_build.get(path),
                    "before_test": fixture_before_test,
                    "after_test": fixture_after_test,
                },
            }
            proof["status"] = (
                "passed"
                if _required_compile_execution_proof_is_valid(
                    {**item, "path": path, "test_execution_proof": proof},
                    root=root,
                    logs_root=logs_root,
                    cargo_authority=cargo_authority,
                )
                else "failed"
            )
            item["test_execution_proof"] = proof
            item["test_execution_valid"] = proof["status"] == "passed"
            item["authority_valid"] = bool(
                item.get("source_authority_valid") is True
                and item.get("cargo_target_valid") is True
                and item.get("test_execution_valid") is True
            )
            item["authority_status"] = (
                "valid" if item["authority_valid"] else "cargo_test_execution_unproven"
            )
            test_reports.append({"path": path, **proof})

    for path in rejected:
        item = required_items[path]
        proof = {
            "status": "failed",
            "path": path,
            "test_id": (item.get("authority") or {}).get("test_id"),
            "authority_path": ((item.get("authority") or {}).get("assertion") or {}).get("path"),
            "fixture_path": path,
            "cargo_metadata": metadata_record,
            "reason": item.get("authority_status"),
        }
        item["test_execution_proof"] = proof
        item["test_execution_valid"] = False
        test_reports.append(proof)

    passed = (
        len(test_reports) == len(required_items)
        and all(item.get("test_execution_valid") is True for item in required_items.values())
    )
    return {
        "status": "passed" if passed else "failed",
        "cargo_metadata": metadata_record,
        "targets": target_reports,
        "tests": test_reports,
    }


def _run_expected_rejection_authority_tests(
    root: Path,
    logs_root: Path,
    sources: Sequence[Mapping[str, Any]],
    expectations: Mapping[str, dict[str, Any]],
    timeout: float,
    cargo_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build each Cargo test target once, then run each exact test ID serially."""
    source_by_path = {str(source.get("path")): source for source in sources}
    rejection_items = {
        path: item
        for path, item in expectations.items()
        if isinstance(item, dict) and item.get("class") == "expected_build_rejection"
    }
    if not rejection_items:
        return {"status": "not_required", "cargo_metadata": None, "targets": [], "tests": []}

    if cargo_authority is None:
        cargo_authority = cargo_test_toolchain_authority(root)
    metadata_result, metadata_document = _cargo_metadata(root, timeout, cargo_authority)
    metadata_record = {
        "command": metadata_result["command"],
        "cargo_toolchain_identity_sha256": metadata_result["cargo_toolchain_identity_sha256"],
        "cargo_execution": metadata_result["cargo_execution"],
        "cwd": metadata_result["cwd"],
        "run_status": metadata_result["run_status"],
        "exit_code": metadata_result["exit_code"],
        "stdout_raw": metadata_result["stdout_raw"],
        "stderr_raw": metadata_result["stderr_raw"],
    }
    groups: dict[tuple[str, str, str], list[tuple[str, dict[str, Any], dict[str, Any]]]] = {}
    rejected: list[str] = []
    for path, item in sorted(rejection_items.items()):
        item["cargo_metadata"] = metadata_record
        item["cargo_target_valid"] = False
        item["cargo_test_target"] = None
        item["authority_valid"] = False
        item["test_execution_valid"] = False
        source = source_by_path.get(path)
        authority = item.get("authority")
        assertion = authority.get("assertion") if isinstance(authority, Mapping) else None
        if (
            source is None
            or item.get("source_authority_valid") is not True
            or not isinstance(authority, Mapping)
            or not isinstance(assertion, Mapping)
            or metadata_result.get("run_status") != "completed"
            or metadata_result.get("exit_code") != 0
        ):
            item["authority_status"] = "cargo_target_or_source_contract_unproven"
            rejected.append(path)
            continue
        test_id = authority.get("test_id")
        authority_path = assertion.get("path")
        target = _cargo_test_target_for_source(metadata_document, root, str(authority_path))
        if not isinstance(test_id, str) or target is None:
            item["authority_status"] = "not_a_cargo_test_target"
            rejected.append(path)
            continue
        item["cargo_test_target"] = target
        item["cargo_target_valid"] = True
        item["authority_status"] = "valid_cargo_test_target"
        key = (str(target["manifest_path"]), str(target["target_name"]), str(target["package_id"]))
        groups.setdefault(key, []).append((path, item, target))

    target_reports: list[dict[str, Any]] = []
    test_reports: list[dict[str, Any]] = []
    for target_index, (key, entries) in enumerate(sorted(groups.items()), 1):
        manifest_path, target_name, _package_id = key
        fixture_before = {
            path: _regular_file_sha256(root / path) for path, _item, _target in entries
        }
        assertion_paths = {
            str(item["authority"]["assertion"]["path"])
            for _path, item, _target in entries
        }
        authority_before = {
            path: _regular_file_sha256(root / path) for path in assertion_paths
        }
        build_command = [
            str(cargo_authority.get("cargo", {}).get("path")), "test", "--manifest-path", manifest_path,
            "--test", target_name, "--no-run",
        ]
        build_result = _run_cargo_authority_command(
            build_command, root, timeout, cargo_authority
        )
        fixture_after_build = {
            path: _regular_file_sha256(root / path) for path, _item, _target in entries
        }
        authority_after_build = {
            path: _regular_file_sha256(root / path) for path in assertion_paths
        }
        target_stem = sha256_bytes("\0".join(key).encode("utf-8"))[:12]
        build_record = _authority_command_record(
            build_result, logs_root,
            f"expected-rejection-target-{target_index:03d}-{target_stem}.build",
        )
        target_reports.append(
            {
                "target": entries[0][2],
                "build": build_record,
                "fixture_sha256_before": fixture_before,
                "fixture_sha256_after": fixture_after_build,
                "authority_sha256_before": authority_before,
                "authority_sha256_after": authority_after_build,
            }
        )
        for test_index, (path, item, target) in enumerate(entries, 1):
            authority = item["authority"]
            assertion = authority["assertion"]
            test_id = str(authority["test_id"])
            authority_path = str(assertion["path"])
            run_command = [
                str(cargo_authority.get("cargo", {}).get("path")), "test", "--manifest-path", manifest_path,
                "--test", target_name, test_id, "--", "--exact", "--nocapture",
            ]
            run_record: dict[str, Any]
            source_before_test = _regular_file_sha256(root / authority_path)
            fixture_before_test = _regular_file_sha256(root / path)
            if build_record.get("run_status") == "completed" and build_record.get("exit_code") == 0:
                run_result = _run_cargo_authority_command(
                    run_command, root, timeout, cargo_authority
                )
                run_record = _authority_command_record(
                    run_result,
                    logs_root,
                    f"expected-rejection-target-{target_index:03d}-{target_stem}.test-{test_index:03d}",
                )
            else:
                run_record = {
                    "command": run_command,
                    "command_sha256": sha256_bytes(
                        json.dumps(run_command, separators=(",", ":")).encode("utf-8")
                    ),
                    "cwd": str(root),
                    "run_status": "not_run_target_build_failed",
                    "exit_code": None,
                    "stdout_raw": None,
                    "stderr_raw": None,
                }
            source_after_test = _regular_file_sha256(root / authority_path)
            fixture_after_test = _regular_file_sha256(root / path)
            proof = {
                "status": "pending",
                "cargo_toolchain_identity_sha256": cargo_authority.get("identity_sha256"),
                "test_id": test_id,
                "authority_path": authority_path,
                "fixture_path": path,
                "cargo_metadata": metadata_record,
                "cargo_test_target": target,
                "target_build": build_record,
                "exact_test_run": run_record,
                "authority_source_hashes": {
                    "expected": assertion.get("sha256"),
                    "before_build": authority_before.get(authority_path),
                    "after_build": authority_after_build.get(authority_path),
                    "before_test": source_before_test,
                    "after_test": source_after_test,
                },
                "fixture_source_hashes": {
                    "path": path,
                    "expected": item.get("source_sha256"),
                    "before_build": fixture_before.get(path),
                    "after_build": fixture_after_build.get(path),
                    "before_test": fixture_before_test,
                    "after_test": fixture_after_test,
                },
            }
            proof["status"] = (
                "passed"
                if _expected_rejection_execution_proof_is_valid(
                    {**item, "path": path, "test_execution_proof": proof},
                    root=root,
                    logs_root=logs_root,
                    cargo_authority=cargo_authority,
                )
                else "failed"
            )
            item["test_execution_proof"] = proof
            item["test_execution_valid"] = proof["status"] == "passed"
            item["authority_valid"] = bool(
                item.get("source_authority_valid") is True
                and item.get("cargo_target_valid") is True
                and item.get("test_execution_valid") is True
            )
            item["authority_status"] = (
                "valid" if item["authority_valid"] else "cargo_test_execution_unproven"
            )
            test_reports.append({"path": path, **proof})

    for path in rejected:
        item = rejection_items[path]
        proof = {
            "status": "failed",
            "path": path,
            "test_id": (item.get("authority") or {}).get("test_id"),
            "authority_path": ((item.get("authority") or {}).get("assertion") or {}).get("path"),
            "fixture_path": path,
            "cargo_metadata": metadata_record,
            "reason": item.get("authority_status"),
        }
        item["test_execution_proof"] = proof
        item["test_execution_valid"] = False
        test_reports.append(proof)

    passed = (
        len(test_reports) == len(rejection_items)
        and all(item.get("test_execution_valid") is True for item in rejection_items.values())
    )
    return {
        "status": "passed" if passed else "failed",
        "cargo_metadata": metadata_record,
        "targets": target_reports,
        "tests": test_reports,
    }


def _write_json_exclusive(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    data = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False).encode("utf-8") + b"\n"
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def _ensure_report_output_is_not_mimi_source(
    root: Path, requested_lexical: Path, resolved_output: Path
) -> None:
    """Prevent a census report from becoming an in-scope Mimi source path."""
    try:
        requested_lexical.relative_to(root)
        lexical_inside_root = True
    except ValueError:
        lexical_inside_root = False
    try:
        resolved_output.relative_to(root)
        resolved_inside_root = True
    except ValueError:
        resolved_inside_root = False
    if (
        lexical_inside_root and requested_lexical.suffix == ".mimi"
    ) or (resolved_inside_root and resolved_output.suffix == ".mimi"):
        raise ValueError(
            f"refusing to place the route census report over an in-scope Mimi path: {resolved_output}"
        )


def _write_journal_line(path: Path, value: Mapping[str, Any]) -> None:
    data = json.dumps(value, sort_keys=True, ensure_ascii=False).encode("utf-8") + b"\n"
    with path.open("ab") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def _run_bounded_parallel_ordered(
    sources: Sequence[Mapping[str, Any]],
    *,
    jobs: int,
    worker: Callable[[int, Mapping[str, Any]], dict[str, Any]],
    on_worker_error: Callable[[int, Mapping[str, Any], Exception], dict[str, Any]],
    on_commit: Callable[[int, Mapping[str, Any], dict[str, Any]], None],
) -> tuple[list[dict[str, Any]], bool]:
    """Run inputs concurrently while committing at most ``jobs`` uncommitted rows.

    Input indexes are one-based manifest positions. The coordinator only waits
    for, records, and reports the next manifest position, even when a later
    worker finishes first. No more work is submitted until that row is
    committed, so completed-but-uncommitted results stay within the same bound.
    """
    if not 1 <= jobs <= 8:
        raise ValueError("worker count must be between 1 and 8")
    if not sources:
        return [], False

    worker_count = min(jobs, len(sources))
    records: list[dict[str, Any]] = []
    saw_worker_error = False
    pending: dict[int, Future[dict[str, Any]]] = {}
    next_to_submit = 1
    next_to_commit = 1

    with ThreadPoolExecutor(max_workers=worker_count, thread_name_prefix="route-census") as executor:
        def submit(index: int) -> None:
            pending[index] = executor.submit(worker, index, sources[index - 1])

        while next_to_submit <= worker_count:
            submit(next_to_submit)
            next_to_submit += 1

        while next_to_commit <= len(sources):
            source = sources[next_to_commit - 1]
            future = pending.pop(next_to_commit)
            try:
                record = future.result()
            except Exception as error:
                saw_worker_error = True
                record = on_worker_error(next_to_commit, source, error)

            on_commit(next_to_commit, source, record)
            records.append(record)
            next_to_commit += 1

            if next_to_submit <= len(sources):
                submit(next_to_submit)
                next_to_submit += 1

    return records, saw_worker_error


def _worker_exception_record(
    *,
    source: Mapping[str, Any],
    index: int,
    logs_root: Path,
    error: Exception,
    expected: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    """Materialize an unexpected worker exception as an explicitly failed row."""
    exception_text = f"{type(error).__name__}: {error}"
    stderr_bytes = ("route census worker failed: " + exception_text + "\n").encode(
        "utf-8", errors="replace"
    )
    # Keep these separate from normal build logs in case _run_one failed after
    # writing one of its ordinary sidecars.
    stem = f"{index:06d}.worker-error"
    stdout_artifact = _write_raw_artifact(logs_root / f"{stem}.stdout.bin", b"")
    stderr_artifact = _write_raw_artifact(logs_root / f"{stem}.stderr.bin", stderr_bytes)
    stdout_text = ""
    stderr_text = _decode(stderr_bytes)
    route = _route_observation(stdout_text, stderr_text, None, "worker_error")
    failure = _failure_classification(
        run_status="worker_error",
        exit_code=None,
        text=stderr_text,
        target_kind=str(source.get("target_kind", "unknown")),
        route=route,
    )
    expectation = _expectation_result(
        str(source["path"]),
        None,
        route,
        expected,
        run_status="worker_error",
        failure_classification=failure,
        captured_output=stderr_bytes,
        output_artifact=None,
    )
    candidate_classification = _route_candidate_classification(route, expectation)
    record: dict[str, Any] = {
        **dict(source),
        "input_index": index,
        "run_status": "worker_error",
        "command": None,
        "exit_code": None,
        "started_unix_seconds": time.time(),
        "elapsed_seconds": 0,
        "stdout": stdout_text,
        "stderr": stderr_text,
        "stdout_raw": stdout_artifact,
        "stderr_raw": stderr_artifact,
        "route": route,
        "failure_classification": failure,
        "expectation": expectation,
        "candidate_classification": candidate_classification,
        "dispatch_stats": {"status": "not_emitted", "files": [], "parsed": None},
        "per_function": {
            "status": "unavailable",
            "named_roster_complete": False,
            "has_fallback": False,
            "limitation": "worker failed before a complete build observation was produced",
        },
        "output_artifact": None,
        "source_hash_before_execution": None,
        "source_hash_after_execution": None,
        "source_stable_for_input": False,
        "worker_exception": {"type": type(error).__name__, "message": str(error)},
        "scratch_cleanup": {"status": "unknown_worker_exception"},
    }
    record["failure_originals"] = {
        "stdout": stdout_text,
        "stderr": stderr_text,
        "exit_code": None,
        "failure_classification": failure,
    }
    return record


def _run_one(
    *,
    root: Path,
    source: Mapping[str, Any],
    index: int,
    mimi: Path,
    timeout: float,
    scratch_root: Path,
    logs_root: Path,
    expected: Mapping[str, Any],
    actor_contracts: Mapping[str, Mapping[str, Any]] | None = None,
) -> dict[str, Any]:
    started = time.time()
    source_path = root / source["path"]
    observed_hash_before: str | None = None
    source_is_symlink = source.get("presence_kind") == "symlink"
    source_is_parent_blocked = (
        source.get("presence_kind") == "blocked_by_symlinked_parent"
    )
    if source.get("buildable", True) and not source_is_symlink and not source_is_parent_blocked:
        try:
            observed_hash_before = sha256_file(source_path)
        except OSError:
            pass
    input_missing = not source_is_parent_blocked and not source.get("present", True)
    input_changed_before = (
        not input_missing
        and not source_is_symlink
        and not source_is_parent_blocked
        and source.get("buildable", True)
        and observed_hash_before != source.get("sha256")
    )
    input_expectation = expected.get(str(source["path"]), {})
    interpreter_only_skip = (
        input_expectation.get("class") == "interpreter_only"
        and input_expectation.get("explicit") is True
        and input_expectation.get("source_hash_status") == "matched"
        and input_expectation.get("authority_valid") is True
    )
    build_not_run_reason = (
        str(input_expectation.get("reason") or "interpreter-only input")
        if interpreter_only_skip
        else None
    )
    scratch = scratch_root / f"input-{index:06d}"
    scratch.mkdir()
    stat_dir = scratch / "dispatch-stats"
    stat_dir.mkdir()
    temp_dir = scratch / "tmp"
    temp_dir.mkdir()
    output_path = scratch / ("out.so" if source["target_kind"] == "shared_library_probe" else "out")
    command: list[str] | None = None
    run_status = "completed"
    stdout_bytes = b""
    stderr_bytes = b""
    exit_code: int | None
    if source_is_symlink:
        run_status = "source_symlink_not_followed"
        exit_code = None
        stderr_bytes = b"Mimi source symlink was recorded but not followed\n"
    elif source_is_parent_blocked:
        run_status = "input_blocked_by_symlinked_parent"
        exit_code = None
        stderr_bytes = (
            "tracked Mimi source is unavailable behind symlinked parent "
            f"{source.get('symlink_parent_path')}\n"
        ).encode("utf-8")
    elif input_missing:
        run_status = "input_missing_from_worktree"
        exit_code = None
        stderr_bytes = b"tracked Mimi source is absent from the worktree\n"
    elif input_changed_before:
        run_status = "input_changed_before_execution"
        exit_code = None
        stderr_bytes = (
            f"source hash changed after initial snapshot: expected {source.get('sha256')}, got {observed_hash_before}\n"
        ).encode("utf-8")
    elif interpreter_only_skip:
        run_status = "not_run"
        exit_code = None
    else:
        command = [str(mimi.resolve()), "build", str(source_path), "-o", str(output_path)]
        if source["target_kind"] == "shared_library_probe":
            command.insert(2, "--shared")
        env = os.environ.copy()
        env["MIMI_STAT"] = "1"
        env["MIMI_VERBOSE"] = "1"
        env["MIMI_ROUTE_CENSUS"] = "1"
        env.pop("MIMI_REACHABLE_DISPATCH", None)
        env.pop("MIMI_RESOLVED_MODULE_BODIES", None)
        env["MIMI_STAT_OUT"] = str(stat_dir)
        env["TMPDIR"] = str(temp_dir)
        try:
            proc = subprocess.run(
                command,
                cwd=root,
                env=env,
                capture_output=True,
                timeout=timeout,
                check=False,
            )
            stdout_bytes = proc.stdout or b""
            stderr_bytes = proc.stderr or b""
            exit_code = proc.returncode
        except subprocess.TimeoutExpired as error:
            run_status = "timeout"
            stdout_bytes = error.output or b""
            stderr_bytes = error.stderr or b""
            exit_code = None
        except OSError as error:
            run_status = "spawn_error"
            stderr_bytes = str(error).encode("utf-8", errors="replace")
            exit_code = None
    log_stem = f"{index:06d}"
    stdout_artifact = _write_raw_artifact(logs_root / f"{log_stem}.stdout.bin", stdout_bytes)
    stderr_artifact = _write_raw_artifact(logs_root / f"{log_stem}.stderr.bin", stderr_bytes)
    stdout_text = _decode(stdout_bytes)
    stderr_text = _decode(stderr_bytes)
    observed_hash_after: str | None = None
    if source.get("buildable", True) and not source_is_symlink:
        try:
            observed_hash_after = sha256_file(source_path)
        except OSError:
            pass
    source_stable_for_input = (
        source.get("buildable", True)
        and observed_hash_before == source.get("sha256")
        and observed_hash_after == source.get("sha256")
    )
    stats_result = _load_dispatch_stats(stat_dir)
    route = _route_observation(stdout_text, stderr_text, exit_code, run_status)
    function_details = _function_observations(
        stdout_text,
        stderr_text,
        stats_result,
        route,
    )
    if run_status == "not_run":
        failure = {
            "class": "none",
            "status": "not_applicable",
            "reason": "build not run for a validated interpreter-only expectation",
            "evidence": None,
        }
    else:
        failure = _failure_classification(
            run_status=run_status,
            exit_code=exit_code,
            text=stderr_text + "\n" + stdout_text,
            target_kind=source["target_kind"],
            route=route,
        )
    output_artifact = _output_artifact_observation(output_path)
    executable_run: dict[str, Any] | None = None
    source_name = str(source.get("path", ""))
    if source_name in ACTOR_FALLBACK_INPUTS:
        contracts = actor_contracts or actor_fixture_test_contracts(root)
        actor_contract = contracts.get(source_name, {})
        expected_native_stdout = actor_contract.get(
            "expected_stdout", _raw_capture(ACTOR_EXPECTED_STDOUT[source_name])
        )
        native_run_status = "not_run_build_unsuccessful"
        native_exit_code: int | None = None
        native_stdout_bytes = b""
        native_stderr_bytes = b""
        if run_status == "completed" and exit_code == 0:
            if isinstance(output_artifact, Mapping) and _is_sha256(output_artifact.get("sha256")):
                try:
                    native_proc = subprocess.run(
                        [str(output_path)],
                        cwd=root,
                        capture_output=True,
                        timeout=timeout,
                        check=False,
                    )
                    native_run_status = "completed"
                    native_exit_code = native_proc.returncode
                    native_stdout_bytes = native_proc.stdout or b""
                    native_stderr_bytes = native_proc.stderr or b""
                except subprocess.TimeoutExpired as error:
                    native_run_status = "timeout"
                    native_stdout_bytes = error.output or b""
                    native_stderr_bytes = error.stderr or b""
                except OSError as error:
                    native_run_status = "spawn_error"
                    native_stderr_bytes = str(error).encode("utf-8", errors="replace")
            else:
                native_run_status = "not_run_output_not_regular_executable"
        native_stem = f"{index:06d}.native"
        native_stdout_artifact = _write_raw_artifact(
            logs_root / f"{native_stem}.stdout.bin", native_stdout_bytes
        )
        native_stderr_artifact = _write_raw_artifact(
            logs_root / f"{native_stem}.stderr.bin", native_stderr_bytes
        )
        expected_native_bytes = _captured_bytes(expected_native_stdout)
        actual_native_bytes = _captured_bytes(native_stdout_artifact)
        executable_run = {
            "run_status": native_run_status,
            "exit_code": native_exit_code,
            "stdout": _decode(native_stdout_bytes),
            "stderr": _decode(native_stderr_bytes),
            "stdout_raw": native_stdout_artifact,
            "stderr_raw": native_stderr_artifact,
            "test_authority_valid": actor_contract.get("authority_valid") is True,
            "test_authority": actor_contract.get("authority"),
            "expected_stdout": expected_native_stdout,
            "stdout_matches_exactly": (
                expected_native_bytes is not None
                and actual_native_bytes is not None
                and actual_native_bytes == expected_native_bytes
            ),
            "exit_code_matches": native_run_status == "completed" and native_exit_code == 0,
        }
    expectation = _expectation_result(
        source["path"],
        exit_code,
        route,
        expected,
        run_status=run_status,
        failure_classification=failure,
        captured_output=stdout_bytes + b"\n" + stderr_bytes,
        output_artifact=output_artifact,
        executable_run=executable_run,
    )
    candidate_classification = _route_candidate_classification(route, expectation)
    record: dict[str, Any] = {
        **dict(source),
        "input_index": index,
        "run_status": run_status,
        "command": command if not input_changed_before else None,
        "exit_code": exit_code,
        "build_not_run_reason": build_not_run_reason if run_status == "not_run" else None,
        "started_unix_seconds": started,
        "elapsed_seconds": round(time.time() - started, 6),
        "stdout": stdout_text,
        "stderr": stderr_text,
        "stdout_raw": stdout_artifact,
        "stderr_raw": stderr_artifact,
        "route": route,
        "failure_classification": failure,
        "expectation": expectation,
        "candidate_classification": candidate_classification,
        "dispatch_stats": stats_result,
        "per_function": function_details,
        "output_artifact": output_artifact,
        "native_executable_run": executable_run,
        "source_hash_before_execution": observed_hash_before,
        "source_hash_after_execution": observed_hash_after,
        "source_stable_for_input": source_stable_for_input,
    }
    record["failure_originals"] = {
        "stdout": stdout_text,
        "stderr": stderr_text,
        "exit_code": exit_code,
        "failure_classification": failure,
    }
    try:
        shutil.rmtree(scratch)
        record["scratch_cleanup"] = {"status": "removed"}
    except OSError as error:
        record["scratch_cleanup"] = {"status": "failed", "error": str(error)}
    return record


def _gate_report(
    records: Sequence[Mapping[str, Any]],
    manifest: Sequence[Mapping[str, Any]],
    snapshot_stable: bool,
    excluded_symlinks: Sequence[Mapping[str, Any]],
    tracked_inventory: Mapping[str, Any],
    actor_fallbacks: Mapping[str, Any],
    legacy_owner_audit: Mapping[str, Any],
    expectation_contract: Mapping[str, Any] | None = None,
    scope_exclusions: Sequence[Mapping[str, Any]] = (),
    source_scope: Mapping[str, Any] | None = None,
    logs_root: Path | None = None,
    root: Path = ROOT,
    expected_rejection_source_root: Path | None = None,
    cargo_authority: Mapping[str, Any] | None = None,
    expected_rejection_cargo_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    if expected_rejection_cargo_authority is None:
        expected_rejection_cargo_authority = cargo_authority
    def nested(record: Mapping[str, Any], key: str) -> Mapping[str, Any]:
        value = record.get(key)
        return value if isinstance(value, Mapping) else {}

    def raw_capture_is_complete(value: Any) -> bool:
        return _captured_bytes(value) is not None and _raw_sidecar_matches(logs_root, value)

    def expected_rejection_is_proven(record: Mapping[str, Any]) -> bool:
        expectation = nested(record, "expectation")
        route = nested(record, "route")
        failure = nested(record, "failure_classification")
        expected = nested(expectation, "expected")
        expected_route = nested(expected, "route")
        route_contract = nested(expectation, "route_contract")
        diagnostic_contract = nested(expectation, "diagnostic_contract")
        return bool(
            expectation.get("class") == "expected_build_rejection"
            and expectation.get("matched") is True
            and expectation.get("status") == "matched"
            and expectation.get("explicit_expectation") is True
            and expectation.get("source_hash_status") == "matched"
            and expectation.get("authority_valid") is True
            and expectation.get("test_contract_valid") is True
            and expectation.get("cargo_target_valid") is True
            and expectation.get("test_execution_valid") is True
            and _expected_rejection_execution_proof_is_valid(
                expectation,
                root=expected_rejection_source_root or root,
                logs_root=logs_root,
                cargo_authority=expected_rejection_cargo_authority,
            )
            and record.get("run_status") == "completed"
            and record.get("exit_code") == expected.get("exit_code")
            and isinstance(expected.get("exit_code"), int)
            and not isinstance(expected.get("exit_code"), bool)
            and expected.get("exit_code", 0) > 0
            and route_contract.get("valid") is True
            and route_contract.get("disposition_matches") is True
            and route_contract.get("classification_status_matches") is True
            and route_contract.get("reason_matches_exactly") is True
            and route_contract.get("failure_class_matches") is True
            and expected_route.get("disposition") == route.get("disposition") == "rejected"
            and expected_route.get("classification_status") == route.get("classification_status") == "observed"
            and expected_route.get("reason") == route.get("reason")
            and expected.get("failure_class") == failure.get("class")
            and failure.get("status") == "classified"
            and diagnostic_contract.get("valid") is True
            and diagnostic_contract.get("diagnostic_codes_match_exactly") is True
            and diagnostic_contract.get("required_output_substrings_match") is True
            and diagnostic_contract.get("output_artifact_absent") is True
            and record.get("output_artifact") is None
            and not nested(record, "per_function").get("has_fallback", False)
            and record.get("source_stable_for_input") is True
        )

    def required_compile_authority_is_proven(record: Mapping[str, Any]) -> bool:
        expectation = nested(record, "expectation")
        if (
            expectation.get("class") != "required_compile"
            or expectation.get("test_contract_valid") is not True
            or expectation.get("authority_valid") is not True
            or expectation.get("source_hash_status") != "matched"
        ):
            return False
        if expectation.get("actor_specific_authority") is True:
            # Actor helper execution is independently rechecked below by the
            # pinned actor test runner and its fixture/helper hash contract.
            return True
        return bool(
            expectation.get("cargo_target_valid") is True
            and expectation.get("test_execution_valid") is True
            and _required_compile_execution_proof_is_valid(
                expectation,
                root=root,
                logs_root=logs_root,
                cargo_authority=expected_rejection_cargo_authority,
            )
        )

    manifest_paths = [str(record.get("path")) for record in manifest]
    record_paths = [str(record.get("path")) for record in records]
    expected_paths = set(manifest_paths)
    actual_paths = set(record_paths)
    duplicate_record_paths = sorted(
        path for path, count in Counter(record_paths).items() if count > 1
    )
    missing_records = sorted(expected_paths - actual_paths)
    unexpected_records = sorted(actual_paths - expected_paths)
    classified_records = [
        (
            record,
            str(
                record.get("candidate_classification")
                or _route_candidate_classification(
                    nested(record, "route"), nested(record, "expectation")
                )
            ),
        )
        for record in records
    ]
    candidate_classification_counts = Counter(
        classification for _record, classification in classified_records
    )
    unclassified_candidates = [
        str(record.get("path"))
        for record, classification in classified_records
        if classification not in ROUTE_CANDIDATE_CLASSIFICATIONS
    ]
    unmapped_candidates = [
        str(record.get("path"))
        for record, classification in classified_records
        if classification in {"unmapped", "unknown"}
    ]
    intentional_rejections = {
        str(record.get("path"))
        for record, classification in classified_records
        if classification == "intentional_reject"
        and expected_rejection_is_proven(record)
    }
    noncanonical_routes = [
        str(record.get("path"))
        for record in records
        if nested(record, "route").get("disposition") != "canonical"
    ]
    unauthorized_noncanonical_routes = [
        str(record.get("path"))
        for record in records
        if nested(record, "route").get("disposition") != "canonical"
        and not expected_rejection_is_proven(record)
    ]
    unclassified_failures = [
        str(record.get("path"))
        for record in records
        if nested(record, "failure_classification").get("class") not in {
            "capability", "library_target", "environment", "external_dependency",
            "source_diagnostic", "unknown", "none"
        }
        or nested(record, "failure_classification").get("class") == "unknown"
        or nested(record, "failure_classification").get("status") == "unclassified"
        or (
            record.get("exit_code") == 0
            and (
                nested(record, "failure_classification").get("class") != "none"
                or nested(record, "failure_classification").get("status") != "not_applicable"
            )
        )
        or (
            record.get("exit_code") not in (None, 0)
            and (
                nested(record, "failure_classification").get("class") == "none"
                or nested(record, "failure_classification").get("status") != "classified"
            )
        )
    ]
    per_function_incomplete = [
        str(record.get("path"))
        for record in records
        if not expected_rejection_is_proven(record)
        and not nested(record, "per_function").get("named_roster_complete", False)
    ]
    fallback_inputs = [
        str(record.get("path"))
        for record in records
        if nested(record, "route").get("disposition") == "compatibility"
        or nested(record, "per_function").get("has_fallback", False)
    ]
    missing_output_artifacts = [
        record["path"]
        for record in records
        if record.get("run_status") == "completed"
        and record.get("exit_code") == 0
        and not (
            isinstance(record.get("output_artifact"), Mapping)
            and isinstance(record["output_artifact"].get("sha256"), str)
            and bool(re.fullmatch(r"[0-9a-f]{64}", record["output_artifact"]["sha256"]))
            and isinstance(record["output_artifact"].get("bytes"), int)
            and not isinstance(record["output_artifact"].get("bytes"), bool)
            and record["output_artifact"]["bytes"] > 0
        )
    ]
    missing_raw_logs = [
        str(record.get("path"))
        for record in records
        if not all(raw_capture_is_complete(record.get(key)) for key in ("stdout_raw", "stderr_raw"))
    ]
    missing_raw_log_sidecars: list[str] = []
    for record in records:
        captures: list[Any] = [record.get("stdout_raw"), record.get("stderr_raw")]
        native_run = record.get("native_executable_run")
        if isinstance(native_run, Mapping):
            captures.extend([native_run.get("stdout_raw"), native_run.get("stderr_raw")])
        if not all(_raw_sidecar_matches(logs_root, capture) for capture in captures):
            missing_raw_log_sidecars.append(str(record.get("path")))
    actor_test_execution = actor_fallbacks.get("rust_test_execution")
    actor_test_cases = (
        actor_test_execution.get("cases", [])
        if isinstance(actor_test_execution, Mapping)
        else []
    )
    actor_test_pairs = [
        (str(item.get("path")), str(item.get("test_id")))
        for item in actor_test_cases
        if isinstance(item, Mapping)
    ]
    required_actor_test_pairs = [
        (path, ACTOR_AUTHORITY_TEST_IDS[path]) for path in ACTOR_FALLBACK_INPUTS
    ]
    actor_test_execution_by_path = {
        str(item.get("path")): item
        for item in actor_test_cases
        if isinstance(item, Mapping)
    }
    actor_test_execution_valid_by_path: dict[str, bool] = {}
    actor_test_result_sources = {
        str(item.get("path")): item for item in manifest if isinstance(item, Mapping)
    }
    for path in ACTOR_FALLBACK_INPUTS:
        result = actor_test_execution_by_path.get(path)
        source = actor_test_result_sources.get(path)
        actor_case = next(
            (
                case for case in actor_fallbacks.get("cases", [])
                if isinstance(case, Mapping) and case.get("path") == path
            ),
            {},
        )
        contract = {
            "authority_valid": actor_case.get("native_test_authority_valid") is True,
            "authority": actor_case.get("native_test_authority"),
            "helper_name": actor_case.get("native_test_helper_name"),
            "helper_sha256": actor_case.get("native_test_helper_sha256"),
            "helper_sha256_expected": actor_case.get("native_test_helper_sha256_expected"),
            "helper_valid": actor_case.get("native_test_helper_valid") is True,
        }
        actor_test_execution_valid_by_path[path] = _actor_test_execution_valid(
            result,
            path,
            source,
            contract,
            root=root,
            logs_root=logs_root,
            cargo_authority=cargo_authority,
        )
    actor_test_execution_exact = (
        isinstance(actor_test_execution, Mapping)
        and actor_test_execution.get("status") == "passed"
        and len(actor_test_pairs) == len(required_actor_test_pairs)
        and sorted(actor_test_pairs) == sorted(required_actor_test_pairs)
        and all(actor_test_execution_valid_by_path.get(path) for path in ACTOR_FALLBACK_INPUTS)
    )
    if isinstance(actor_test_execution, Mapping):
        for result in actor_test_cases:
            if not isinstance(result, Mapping):
                missing_raw_logs.append("actor-rust-test:invalid-record")
                missing_raw_log_sidecars.append("actor-rust-test:invalid-record")
                continue
            captures = [result.get("stdout_raw"), result.get("stderr_raw")]
            owner = f"actor-rust-test:{result.get('path')}"
            if not all(raw_capture_is_complete(capture) for capture in captures):
                missing_raw_logs.append(owner)
            if not all(_raw_sidecar_matches(logs_root, capture) for capture in captures):
                missing_raw_log_sidecars.append(owner)
    else:
        missing_raw_logs.append("actor-rust-tests")
        missing_raw_log_sidecars.append("actor-rust-tests")
    owner_captures = [
        legacy_owner_audit.get("stdout_raw"),
        legacy_owner_audit.get("stderr_raw"),
    ]
    if not all(raw_capture_is_complete(capture) for capture in owner_captures):
        missing_raw_logs.append("legacy-owner-audit")
    if not all(_raw_sidecar_matches(logs_root, capture) for capture in owner_captures):
        missing_raw_log_sidecars.append("legacy-owner-audit")
    mismatches = [str(record.get("path")) for record in records if not record.get("expectation", {}).get("matched")]
    expectation_paths = [
        str(record.get("path"))
        for record in records
        if isinstance(record.get("expectation"), Mapping)
    ]
    expectation_path_duplicates = sorted(
        name for name, count in Counter(expectation_paths).items() if count > 1
    )
    if expectation_contract is None:
        expectation_contract = {
            "exact_bijection": (
                set(expectation_paths) == expected_paths
                and not expectation_path_duplicates
                and len(expectation_paths) == len(expected_paths)
            ),
            "missing_paths": sorted(expected_paths - set(expectation_paths)),
            "extra_paths": sorted(set(expectation_paths) - expected_paths),
            "duplicate_inventory_paths": [],
            "invalid_source_hash_paths": [],
            "invalid_authority_paths": [],
        }
    all_mapped_expectations_have_authority = all(
        nested(record, "expectation").get("class") == "unmapped"
        or (
            nested(record, "expectation").get("explicit_expectation") is True
            and nested(record, "expectation").get("authority_valid") is True
        )
        for record in records
    ) and not expectation_contract.get("invalid_authority_paths")
    all_expectation_source_hashes_match = all(
        nested(record, "expectation").get("class") == "unmapped"
        or nested(record, "expectation").get("source_hash_status") == "matched"
        for record in records
    ) and not expectation_contract.get("invalid_source_hash_paths")
    expected_negative_matches = sorted(
        str(record.get("path"))
        for record in records
        if nested(record, "expectation").get("class")
        in {"expected_build_rejection", "environment_failure"}
        and nested(record, "expectation").get("matched") is True
    )
    passed_expected_rejections = sorted(
        str(record.get("path"))
        for record in records
        if expected_rejection_is_proven(record)
    )
    passed_expected_rejection_set = set(passed_expected_rejections)
    canonical_success_inputs = sorted(
        str(record.get("path"))
        for record in records
        if nested(record, "expectation").get("class") == "required_compile"
        and nested(record, "expectation").get("matched") is True
        and nested(record, "expectation").get("explicit_expectation") is True
        and nested(record, "expectation").get("source_hash_status") == "matched"
        and required_compile_authority_is_proven(record)
        and record.get("run_status") == "completed"
        and record.get("exit_code") == 0
        and nested(record, "route").get("disposition") == "canonical"
        and nested(record, "route").get("classification_status") == "observed"
        and nested(record, "failure_classification").get("class") == "none"
        and nested(record, "failure_classification").get("status") == "not_applicable"
        and nested(record, "per_function").get("named_roster_complete") is True
        and not nested(record, "per_function").get("has_fallback", False)
        and record.get("source_stable_for_input") is True
        and isinstance(nested(record, "output_artifact").get("sha256"), str)
        and bool(
            re.fullmatch(
                r"[0-9a-f]{64}",
                str(nested(record, "output_artifact").get("sha256", "")),
            )
        )
        and isinstance(nested(record, "output_artifact").get("bytes"), int)
        and not isinstance(nested(record, "output_artifact").get("bytes"), bool)
        and nested(record, "output_artifact").get("bytes", 0) > 0
    )
    canonical_expectation_paths = {
        str(record.get("path"))
        for record in records
        if nested(record, "expectation").get("class") == "required_compile"
    }
    canonical_success_set = set(canonical_success_inputs)
    not_successful = [
        str(record.get("path"))
        for record in records
        if (record.get("run_status") != "completed" or record.get("exit_code") != 0)
        and str(record.get("path")) not in passed_expected_rejection_set
    ]
    unexpected_failure_inputs = [
        str(record.get("path"))
        for record in records
        if (
            record.get("run_status") in {"timeout", "spawn_error"}
            or (
                record.get("run_status") == "completed"
                and record.get("exit_code") not in (None, 0)
            )
        )
        and str(record.get("path")) not in passed_expected_rejection_set
    ]
    unproven_expected_rejections = sorted(
        str(record.get("path"))
        for record in records
        if nested(record, "expectation").get("class") == "expected_build_rejection"
        and not expected_rejection_is_proven(record)
    )
    missing_manifest_inputs = [str(record.get("path")) for record in manifest if not record.get("present", False)]
    tracked_manifest_paths = sorted(
        str(record.get("path"))
        for record in [*manifest, *scope_exclusions]
        if record.get("tracked_by_git")
    )
    tracked_path_count_matches = (
        tracked_inventory.get("path_count") == len(tracked_manifest_paths)
    )
    tracked_path_digest_matches = (
        tracked_inventory.get("paths_sha256") == _manifest_paths_hash(tracked_manifest_paths)
    )
    actor_cases = actor_fallbacks.get("cases", [])
    actor_by_path = {
        str(case.get("path")): case
        for case in actor_cases
        if isinstance(case, Mapping)
    }
    actor_evidence_complete = all(
        actor_by_path.get(path, {}).get("evidence_complete")
        and actor_by_path[path].get("in_denominator")
        and actor_by_path[path].get("present")
        for path in ACTOR_FALLBACK_INPUTS
    )
    actor_no_fallback = all(
        actor_by_path.get(path, {}).get("whole_program_disposition") == "canonical"
        and actor_by_path[path].get("run_status") == "completed"
        and actor_by_path[path].get("exit_code") == 0
        and actor_by_path[path].get("per_function", {}).get("named_roster_complete")
        and not actor_by_path[path].get("per_function", {}).get("has_fallback")
        and actor_by_path[path].get("native_test_authority_valid") is True
        and actor_test_execution_valid_by_path.get(path) is True
        and actor_by_path[path].get("native_executable_contract_matches") is True
        for path in ACTOR_FALLBACK_INPUTS
    )
    actor_invalid_test_authority = [
        path for path in ACTOR_FALLBACK_INPUTS
        if actor_by_path.get(path, {}).get("native_test_authority_valid") is not True
    ]
    actor_native_output_mismatches = [
        path for path in ACTOR_FALLBACK_INPUTS
        if actor_by_path.get(path, {}).get("native_executable_contract_matches") is not True
    ]
    tracked_symlink_parent_blockers = [
        item for item in excluded_symlinks
        if item.get("kind") == "tracked_mimi_path_parent_symlink_not_followed"
    ]
    owner_rows = legacy_owner_audit.get("owners", [])
    owner_names = {
        str(owner.get("name")) for owner in owner_rows if isinstance(owner, Mapping)
    }
    retired_owner_rows = legacy_owner_audit.get("retired_owners", [])
    retired_owner_names = {
        str(owner.get("name"))
        for owner in retired_owner_rows
        if isinstance(owner, Mapping)
    }
    owner_rows_complete = (
        owner_names == set(ACTIVE_LEGACY_OWNER_NAMES)
        and len(owner_rows) == len(ACTIVE_LEGACY_OWNER_NAMES)
        and legacy_owner_audit.get("owner_count_reported") == len(ACTIVE_LEGACY_OWNER_NAMES)
        and retired_owner_names == set(RETIRED_LEGACY_OWNER_NAMES)
        and len(retired_owner_rows) == len(RETIRED_LEGACY_OWNER_NAMES)
        and legacy_owner_audit.get("retired_owner_evidence_complete") is True
        and legacy_owner_audit.get("parsed_owner_set_complete") is True
        and legacy_owner_audit.get("deletion_condition_digests_match_text") is True
        and not legacy_owner_audit.get("unknown_owner_names")
        and not legacy_owner_audit.get("duplicate_owner_fields")
        and not legacy_owner_audit.get("malformed_condition_rows")
        and bool(
            re.fullmatch(
                r"[0-9a-f]{64}",
                str(legacy_owner_audit.get("owner_deletion_condition_set_sha256", "")),
            )
        )
        and all(
            isinstance(owner, Mapping)
            and isinstance(owner.get("production_accessor_call_sites"), int)
            and owner.get("production_access_status") in {"sealed", "open"}
            and isinstance(owner.get("dependency_class"), str)
            and isinstance(owner.get("owner_deletion_ready"), bool)
            and isinstance(owner.get("owner_deletion_blocker"), str)
            and bool(owner.get("deletion_condition"))
            and bool(re.fullmatch(r"[0-9a-f]{64}", str(owner.get("deletion_condition_sha256", ""))))
            for owner in owner_rows
        )
        and all(
            isinstance(owner, Mapping)
            and owner.get("status") == "physically_retired"
            and owner.get("retirement_evidence_kind") == "audit_script_source_checks"
            and owner.get("retirement_evidence_complete") is True
            and owner.get("declared_parity_test_names") == list(RETIRED_FLOW_DUAL_PARITY_TESTS)
            and owner.get("dynamic_probe_status") == "not_run"
            for owner in retired_owner_rows
        )
    )
    expected_input_count = len(manifest)
    complete = (
        expected_input_count > 0
        and len(records) == expected_input_count
        and not missing_records
        and not unexpected_records
        and not duplicate_record_paths
    )
    scope_paths = [
        str(record.get("path")) for record in [*manifest, *scope_exclusions]
    ]
    scope_duplicate_paths = sorted(
        name for name, count in Counter(scope_paths).items() if count > 1
    )
    scope_is_exact = (
        source_scope.get("exactly_once") is True
        if isinstance(source_scope, Mapping)
        else True
    ) and not scope_duplicate_paths
    classification_exact = (
        sum(candidate_classification_counts.values()) == len(records)
        and not unclassified_candidates
        and len(record_paths) == len(set(record_paths))
    )
    checks = {
        "cargo_test_toolchain_identity_is_current_and_unmodified": (
            _cargo_toolchain_identity_valid(cargo_authority, root=root)
        ),
        "expected_rejection_cargo_toolchain_identity_is_current_and_unmodified": (
            _cargo_toolchain_identity_valid(
                expected_rejection_cargo_authority,
                root=expected_rejection_source_root or root,
            )
            if any(
                nested(record, "expectation").get("class") == "expected_build_rejection"
                for record in records
            )
            else True
        ),
        "nonempty_exact_input_denominator": complete,
        "source_scope_is_accounted_exactly_once": scope_is_exact,
        "all_candidates_have_one_route_classification": classification_exact,
        "no_unknown_or_unmapped_route_candidates": not unmapped_candidates,
        "tracked_git_manifest_available": (
            tracked_inventory.get("status") == "available"
            and tracked_path_count_matches
            and tracked_path_digest_matches
        ),
        "both_actor_fallback_cases_are_in_denominator": (
            not actor_fallbacks.get("missing_cases") and actor_evidence_complete
        ),
        "both_exact_actor_rust_tests_ran_and_passed": actor_test_execution_exact,
        "actor_fallback_cases_are_canonical_without_fallback": actor_no_fallback,
        "two_active_owners_audited_and_two_retired_owners_statically_verified": (
            bool(legacy_owner_audit.get("accepted_as_complete_inventory")) and owner_rows_complete
        ),
        "no_missing_tracked_inputs": not missing_manifest_inputs,
        "no_unfollowed_mimi_symlinks": not excluded_symlinks,
        "no_tracked_mimi_paths_through_symlinked_parents": not tracked_symlink_parent_blockers,
        "all_routes_are_canonical_or_authorized_rejections": not unauthorized_noncanonical_routes,
        "all_canonical_expectations_are_proven": canonical_expectation_paths <= canonical_success_set,
        "no_legacy_or_emission_fallback": not fallback_inputs,
        "all_per_function_eligibility_is_named_and_reconciled": not per_function_incomplete,
        "all_builds_succeeded_or_rejected_as_declared": not not_successful,
        "no_unexpected_build_failures": not unexpected_failure_inputs,
        "all_expected_rejections_are_proven": all(
            nested(record, "expectation").get("class") != "expected_build_rejection"
            or expected_rejection_is_proven(record)
            for record in records
        ),
        "no_unclassified_failure_causes": not unclassified_failures,
        "all_outcomes_match_expectations": not mismatches,
        "expectation_paths_exactly_biject_inventory": (
            expectation_contract.get("exact_bijection") is True
            and not expectation_path_duplicates
            and not expectation_contract.get("duplicate_inventory_paths")
        ),
        "all_mapped_expectations_have_test_authority": all_mapped_expectations_have_authority,
        "all_mapped_expectation_source_hashes_match": all_expectation_source_hashes_match,
        "all_successes_have_hashed_output_artifacts": not missing_output_artifacts,
        "all_stdout_stderr_logs_are_retained": (
            not missing_raw_logs and not missing_raw_log_sidecars
        ),
        "all_inputs_are_stable_during_their_build": all(
            record.get("source_stable_for_input") is True for record in records
        ),
        "source_snapshot_stable": snapshot_stable,
    }
    return {
        "passed": all(checks.values()),
        "checks": checks,
        "input_count": expected_input_count,
        "outcome_count": len(records),
        "manifest_paths_sha256": _manifest_paths_hash(manifest_paths),
        "manifest_sha256": _manifest_digest(manifest),
        "missing_manifest_records": missing_records,
        "unexpected_records": unexpected_records,
        "duplicate_record_paths": duplicate_record_paths,
        "scope_duplicate_paths": scope_duplicate_paths,
        "scope_exclusion_paths": [str(record.get("path")) for record in scope_exclusions],
        "candidate_classification_counts": dict(sorted(candidate_classification_counts.items())),
        "unclassified_candidates": unclassified_candidates,
        "unknown_or_unmapped_candidates": unmapped_candidates,
        "intentional_reject_candidates": sorted(intentional_rejections),
        "missing_tracked_inputs": missing_manifest_inputs,
        "missing_actor_fallback_cases": list(actor_fallbacks.get("missing_cases", [])),
        "legacy_owner_audit_complete": bool(legacy_owner_audit.get("accepted_as_complete_inventory")),
        "tracked_manifest_path_count_matches": tracked_path_count_matches,
        "tracked_manifest_paths_sha256_matches": tracked_path_digest_matches,
        "actor_fallback_evidence_complete": actor_evidence_complete,
        "actor_fallback_cases_are_canonical_without_fallback": actor_no_fallback,
        "actor_fallback_invalid_test_authority": actor_invalid_test_authority,
        "actor_fallback_native_output_mismatches": actor_native_output_mismatches,
        "actor_rust_test_execution_failures": [
            path for path in ACTOR_FALLBACK_INPUTS
            if actor_test_execution_valid_by_path.get(path) is not True
        ],
        "legacy_owner_rows_complete": owner_rows_complete,
        "unfollowed_mimi_symlinks": [item["path"] for item in excluded_symlinks],
        "tracked_symlink_parent_blockers": tracked_symlink_parent_blockers,
        "noncanonical_route_inputs": noncanonical_routes,
        "unauthorized_noncanonical_route_inputs": unauthorized_noncanonical_routes,
        "fallback_inputs": fallback_inputs,
        "per_function_incomplete_inputs": per_function_incomplete,
        "unclassified_failure_inputs": unclassified_failures,
        "unexpected_or_mismatched_inputs": mismatches,
        "expectation_path_coverage": {
            "exact_bijection": expectation_contract.get("exact_bijection") is True,
            "missing_paths": list(expectation_contract.get("missing_paths", [])),
            "extra_paths": list(expectation_contract.get("extra_paths", [])),
            "duplicate_record_paths": expectation_path_duplicates,
            "duplicate_inventory_paths": list(
                expectation_contract.get("duplicate_inventory_paths", [])
            ),
        },
        "invalid_expectation_source_hash_inputs": list(
            expectation_contract.get("invalid_source_hash_paths", [])
        ),
        "invalid_expectation_authority_inputs": list(
            expectation_contract.get("invalid_authority_paths", [])
        ),
        "expected_negative_matches": expected_negative_matches,
        "passed_expected_rejections": passed_expected_rejections,
        "unproven_expected_rejection_inputs": unproven_expected_rejections,
        "unexpected_failure_inputs": unexpected_failure_inputs,
        "canonical_success_inputs": canonical_success_inputs,
        "canonical_success_count": len(canonical_success_inputs),
        "not_successful_inputs": not_successful,
        "missing_output_artifact_inputs": missing_output_artifacts,
        "missing_raw_log_inputs": missing_raw_logs,
        "missing_raw_log_sidecars": missing_raw_log_sidecars,
    }


def _write_inventory_artifacts(
    artifact_root: Path,
    root: Path,
    sources: Sequence[Mapping[str, Any]],
    excluded_symlinks: Sequence[Mapping[str, Any]],
    snapshot_id: str,
    tracked_inventory: Mapping[str, Any],
    scope_exclusions: Sequence[Mapping[str, Any]],
    source_scope: Mapping[str, Any],
) -> None:
    artifact_root.mkdir(parents=True, exist_ok=False)
    (artifact_root / "logs").mkdir()
    manifest_sha256 = _manifest_digest(sources)
    inventory = {
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": "route-census",
        "source_snapshot_id": snapshot_id,
        "manifest_sha256": manifest_sha256,
        "input_count": len(sources),
        "inputs": list(sources),
        "scope_exclusions": list(scope_exclusions),
        "source_scope": dict(source_scope),
        "required_actor_fallback_inputs": list(ACTOR_FALLBACK_INPUTS),
        "legacy_owner_names_requiring_individual_status": list(ACTIVE_LEGACY_OWNER_NAMES),
        "legacy_owner_names_with_static_retirement_evidence": list(RETIRED_LEGACY_OWNER_NAMES),
        "tracked_git_inventory": dict(tracked_inventory),
        "unfollowed_mimi_symlinks": list(excluded_symlinks),
        "inventory_rule": {
            "root": str(root),
            "included": "all active Git-indexed *.mimi paths (including tracked paths missing from disk) plus every present regular or symlink *.mimi outside root-level archival roots devdocs/ and evidence/ and named generated/vendor exclusions",
            "archival_exclusions": "all tracked or present Mimi paths below root-level devdocs/ and evidence/ are retained in scope_exclusions with hashes and reasons, but are not route candidates",
            "excluded_directory_names": sorted(EXCLUDED_DIRS),
            "symlinks": "directory and file symlinks are not followed; every observed source symlink is listed and blocks --check",
        },
    }
    _write_json_exclusive(artifact_root / "inventory.json", inventory)
    with (artifact_root / "journal.jsonl").open("xb") as stream:
        stream.flush()
        os.fsync(stream.fileno())


def collect(args: argparse.Namespace) -> int:
    root = Path(args.root).resolve()
    if not root.is_dir():
        raise ValueError(f"source root is not a directory: {root}")
    mimi = Path(args.mimi).expanduser()
    if not mimi.is_absolute():
        mimi = root / mimi
    requested_output = Path(args.output).expanduser()
    requested_output_lexical = Path(os.path.abspath(requested_output))
    try:
        # Resolve the last path component as well as parent symlinks. This
        # catches external aliases whose target does not exist yet, including
        # an output symlink or a symlinked parent directory.
        output = requested_output.resolve(strict=False)
    except (OSError, RuntimeError) as error:
        raise ValueError(
            f"refusing to use an unresolvable output path: {requested_output}"
        ) from error

    def validate_report_destination() -> None:
        try:
            current_output = requested_output.resolve(strict=False)
        except (OSError, RuntimeError) as error:
            raise ValueError(
                f"refusing to use an unresolvable output path: {requested_output}"
            ) from error
        if current_output != output:
            raise ValueError(
                f"refusing to use a report output path that changed during collection: {requested_output}"
            )
        _ensure_report_output_is_not_mimi_source(
            root, requested_output_lexical, current_output
        )

    validate_report_destination()
    if output.exists():
        raise FileExistsError(f"refusing to overwrite output file: {output}")
    requested_artifacts = (
        Path(args.artifacts).expanduser().resolve()
        if args.artifacts
        else Path(str(output) + ".artifacts")
    )
    if not args.inventory_only and requested_artifacts.exists():
        raise FileExistsError(f"refusing to overwrite artifact directory: {requested_artifacts}")
    tracked_paths, tracked_inventory = tracked_mimi_paths(root)
    sources, scope_exclusions, source_scope = enumerate_source_scope(root, tracked_paths)
    excluded_symlinks = source_symlink_exclusions(root)
    for name in tracked_paths:
        path = root / name
        if path.is_symlink():
            excluded_symlinks.append({"path": name, "kind": "tracked_mimi_file_symlink_not_followed"})
    tracked_parent_blockers = tracked_symlink_parent_blockers(root, tracked_paths)
    excluded_symlinks.extend(tracked_parent_blockers)
    excluded_symlinks = sorted(
        { (entry["path"], entry["kind"]): entry for entry in excluded_symlinks }.values(),
        key=lambda item: (item["path"], item["kind"]),
    )
    snapshot_start = source_snapshot(root, sources)
    manifest_sha256 = _manifest_digest(sources)
    requested_worker_count = getattr(args, "jobs", 4)
    actual_worker_count = (
        0 if args.inventory_only else min(requested_worker_count, len(sources))
    )
    declared_expectations, expectations_identity = load_expectations(
        Path(args.expectations).expanduser().resolve() if args.expectations else None,
        root,
    )
    actor_contracts = actor_fixture_test_contracts(root)
    declared_expectations, actor_default_expectations = add_actor_default_expectations(
        declared_expectations,
        sources,
        root,
    )
    expectations, expectation_contract = bind_expectations_to_inventory(
        declared_expectations,
        sources,
        root,
    )
    cargo_authority = cargo_test_toolchain_authority(
        root, probe_active_toolchain=not args.inventory_only
    )
    base: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": "route-census",
        "created_unix_seconds": time.time(),
        "collector": {
            "path": str(Path(__file__).resolve()),
            "sha256": sha256_file(Path(__file__).resolve()),
        },
        "collection_mode": "inventory_only" if args.inventory_only else "full_execution",
        "input_inventory": {
            "root": str(root),
            "count": len(sources),
            "manifest_sha256": manifest_sha256,
            "tracked_git_inventory": tracked_inventory,
            "category_counts": _count_by(sources, "category"),
            "target_kind_counts": _count_by(sources, "target_kind"),
            "inputs": sources,
            "scope_exclusions": scope_exclusions,
            "source_scope": source_scope,
            "unfollowed_mimi_symlinks": excluded_symlinks,
            "rule": "active tracked *.mimi paths plus present regular Mimi files; devdocs/ and evidence/ are retained in scope_exclusions and are never route candidates",
            "excluded_directory_names": sorted(EXCLUDED_DIRS),
            "archival_prefixes": dict(sorted(ARCHIVAL_SOURCE_PREFIXES.items())),
            "symlinks_excluded": True,
        },
        "source_scope": source_scope,
        "manifest": {
            "sha256": manifest_sha256,
            "denominator": len(sources),
            "entries_field": "input_inventory.inputs",
            "scope_rule_field": "input_inventory.rule",
            "source_scope_exactly_once": source_scope["exactly_once"],
        },
        "expectations": {
            "identity": {
                "provided_manifest": expectations_identity,
                "built_in_actor_defaults": actor_default_expectations,
            },
            **expectation_contract,
            "classes": sorted(EXPECTATION_CLASSES),
            "route_candidate_classifications": sorted(ROUTE_CANDIDATE_CLASSIFICATIONS),
            "missing_expectations_are": "explicit unmapped rows; never implicit success",
            "raw_output_policy": (
                "invoked builds retain exact stdout/stderr; only a validated interpreter_only mapping "
                "selects run_status=not_run with no command or output artifact"
            ),
        },
        "actor_fixture_contracts": actor_contracts,
        "source_snapshot": snapshot_start,
        "repository": repository_identity(root),
        "toolchain": toolchain_manifest(
            mimi,
            probe_cli=not args.inventory_only,
            probe_toolchain_versions=not args.inventory_only,
            root=root,
            cargo_authority=cargo_authority,
        ),
        "cargo_test_toolchain_authority": cargo_authority,
        "execution_contract": {
            "command_template": ["<mimi>", "build", "[--shared for no-main inputs]", "<source>", "-o", "<unique-output>"],
            "route_mode": "default production route; --mir is not passed",
            "per_input_timeout_seconds": args.timeout,
            "parallelism": actual_worker_count,
            "worker_count": actual_worker_count,
            "requested_worker_count": requested_worker_count,
            "submitted_but_uncommitted_window_limit": actual_worker_count,
            "ordered_commit": "journal, final records, and progress are committed in manifest input order",
            "raw_output": "complete UTF-8 text plus exact stdout/stderr bytes in base64 and adjacent .bin files",
            "actor_test_execution": (
                "full execution runs the exact two named real_world_cli Actor tests; command, exit, raw logs, "
                "fixture hashes, and test-source hashes are retained and required by the strict gate"
            ),
            "route_classes": ["canonical", "compatibility", "rejected", "unobserved"],
            "per_function_requirement": "route-specific named emitter disposition for every canonical executable owner; canonical MIR events reconcile with the MIR function count and route digest after module verification, while resolved-native events reconcile with MIMI_STAT and their emitter summary",
            "failure_classes": ["capability", "library_target", "environment", "unknown", "none"],
            "build_skip_rule": "a validated explicit interpreter_only expectation skips invocation and records a reason; this row is never canonical",
        "strict_check": "requires an exact active candidate inventory and source-scope ledger, one route classification per candidate, valid source-bound expectations, a canonical build for required_compile inputs, or a source-bound expected rejection whose active Cargo test target compiled and passed by exact test ID under the recorded absolute Cargo/rustc/rustup dispatch paths and stable before/after executable hashes; Cargo/rustc must resolve through the same native rustup proxy, and inherited compiler, wrapper, bootstrap, toolchain-home, cfg, target, linker, runner, alias, and Rust flag overrides or relevant Cargo config overrides fail closed; no compatibility/function fallback, complete named per-function rosters for canonical builds, classified failures, and a stable source fingerprint",
            "no_production_route_changes": True,
        },
    }
    if args.inventory_only:
        if args.check:
            raise ValueError("--check cannot be combined with --inventory-only")
        base["records"] = [
            _inventory_only_record(record, index, expectations)
            for index, record in enumerate(sources, 1)
        ]
        base["actor_fallback_cases"] = _actor_fallback_evidence(
            sources, [], actor_contracts, root=root, cargo_authority=cargo_authority
        )
        base["legacy_owner_audit"] = {
            "status": "not_run_inventory_only",
            "owners": [
                {"name": name, "status": "not_assessed"}
                for name in ACTIVE_LEGACY_OWNER_NAMES
            ],
            "retired_owners": [
                {"name": name, "status": "unverified"}
                for name in RETIRED_LEGACY_OWNER_NAMES
            ],
        }
        base["candidate_classification_counts"] = _count_by(
            [{"classification": row["candidate_classification"]} for row in base["records"]],
            "classification",
        )
        base["gate"] = {"passed": None, "status": "not_evaluated_inventory_only", "strict_gate_available": False}
        validate_report_destination()
        _write_json_exclusive(output, base)
        print(f"[full-route-census] inventory only: {len(sources)} inputs -> {output}")
        return 0

    artifact_root = requested_artifacts
    _write_inventory_artifacts(
        artifact_root,
        root,
        sources,
        excluded_symlinks,
        snapshot_start["snapshot_id"],
        tracked_inventory,
        scope_exclusions,
        source_scope,
    )
    logs_root = artifact_root / "logs"
    legacy_owner_audit = _collect_legacy_owner_audit(root, logs_root)
    actor_test_execution = _run_actor_authority_tests(
        root, logs_root, sources, actor_contracts, args.timeout, cargo_authority
    )
    base["actor_rust_test_execution"] = actor_test_execution
    expected_rejection_authority_execution = _run_expected_rejection_authority_tests(
        root, logs_root, sources, expectations, args.timeout, cargo_authority
    )
    base["expected_rejection_authority_execution"] = (
        expected_rejection_authority_execution
    )
    required_compile_authority_execution = _run_required_compile_authority_tests(
        root, logs_root, sources, expectations, args.timeout, cargo_authority
    )
    base["required_compile_authority_execution"] = (
        required_compile_authority_execution
    )
    expectation_contract["invalid_authority_paths"] = sorted(
        name
        for name, expectation in expectations.items()
        if expectation.get("class") != "unmapped"
        and expectation.get("authority_valid") is not True
    )
    base["expectations"]["invalid_authority_paths"] = list(
        expectation_contract["invalid_authority_paths"]
    )
    scratch_parent = Path(args.scratch_root).expanduser().resolve() if args.scratch_root else root / "target"
    scratch_parent.mkdir(parents=True, exist_ok=True)
    records: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="full-route-census-", dir=scratch_parent) as temp_name:
        scratch_root = Path(temp_name)
        journal = artifact_root / "journal.jsonl"

        def run_worker(index: int, source: Mapping[str, Any]) -> dict[str, Any]:
            return _run_one(
                root=root,
                source=source,
                index=index,
                mimi=mimi,
                timeout=args.timeout,
                scratch_root=scratch_root,
                logs_root=logs_root,
                expected=expectations,
                actor_contracts=actor_contracts,
            )

        def record_worker_error(
            index: int, source: Mapping[str, Any], error: Exception
        ) -> dict[str, Any]:
            return _worker_exception_record(
                source=source,
                index=index,
                logs_root=logs_root,
                error=error,
                expected=expectations,
            )

        def commit_record(
            index: int, source: Mapping[str, Any], record: dict[str, Any]
        ) -> None:
            _write_journal_line(journal, record)
            print(
                f"[full-route-census] [{index}/{len(sources)}] {source['path']}: "
                f"route={record['route']['disposition']} exit={record['exit_code']} "
                f"expectation={record['expectation']['status']}",
                file=sys.stderr,
            )

        records, saw_worker_error = _run_bounded_parallel_ordered(
            sources,
            jobs=requested_worker_count,
            worker=run_worker,
            on_worker_error=record_worker_error,
            on_commit=commit_record,
        )
        if saw_worker_error:
            print(
                "[full-route-census] worker exception recorded; leaving the run partial without a final report",
                file=sys.stderr,
            )
            return 2
    tracked_paths_end, tracked_inventory_end = tracked_mimi_paths(root)
    snapshot_end = source_snapshot(root, enumerate_sources(root, tracked_paths_end))
    excluded_symlinks_end = source_symlink_exclusions(root)
    for name in tracked_paths_end:
        path = root / name
        if path.is_symlink():
            excluded_symlinks_end.append({"path": name, "kind": "tracked_mimi_file_symlink_not_followed"})
    tracked_parent_blockers_end = tracked_symlink_parent_blockers(root, tracked_paths_end)
    excluded_symlinks_end.extend(tracked_parent_blockers_end)
    excluded_symlinks_end = sorted(
        { (entry["path"], entry["kind"]): entry for entry in excluded_symlinks_end }.values(),
        key=lambda item: (item["path"], item["kind"]),
    )
    snapshot_stable = (
        snapshot_start["snapshot_id"] == snapshot_end["snapshot_id"]
        and excluded_symlinks == excluded_symlinks_end
        and tracked_paths == tracked_paths_end
        and tracked_inventory.get("status") == tracked_inventory_end.get("status") == "available"
    )
    base["source_snapshot_end"] = snapshot_end
    base["tracked_git_inventory_end"] = tracked_inventory_end
    base["unfollowed_mimi_symlinks_end"] = excluded_symlinks_end
    base["source_snapshot_stable"] = snapshot_stable
    base["artifacts"] = {
        "root": str(artifact_root),
        "inventory": "inventory.json",
        "journal": "journal.jsonl",
        "logs": "logs/",
        "retention": "collector does not delete raw logs or partial journal",
    }
    base["records"] = records
    base["actor_fallback_cases"] = _actor_fallback_evidence(
        sources,
        records,
        actor_contracts,
        actor_test_execution,
        root=root,
        cargo_authority=cargo_authority,
    )
    base["legacy_owner_audit"] = legacy_owner_audit
    base["route_counts"] = _count_by(
        [{"route": record["route"]["disposition"]} for record in records], "route"
    )
    base["route_counts_by_category"] = {
        category: _count_by(
            [{"route": record["route"]["disposition"]} for record in records if record["category"] == category],
            "route",
        )
        for category in sorted({str(record["category"]) for record in records})
    }
    base["run_status_counts"] = _count_by(records, "run_status")
    base["failure_class_counts"] = _count_by(
        [{"failure_class": record["failure_classification"]["class"]} for record in records],
        "failure_class",
    )
    base["per_function_roster_counts"] = _count_by(
        [
            {"roster": "complete_named" if record.get("per_function", {}).get("named_roster_complete") else "incomplete"}
            for record in records
        ],
        "roster",
    )
    base["candidate_classification_counts"] = _count_by(
        [
            {
                "classification": record.get("candidate_classification")
                or _route_candidate_classification(
                    record.get("route", {}), record.get("expectation")
                )
            }
            for record in records
        ],
        "classification",
    )
    blocking_symlinks = [
        item for item in excluded_symlinks
        if not _is_archival_source_path(str(item.get("path", "")))
    ]
    base["gate"] = _gate_report(
        records,
        sources,
        snapshot_stable,
        blocking_symlinks,
        tracked_inventory,
        base["actor_fallback_cases"],
        legacy_owner_audit,
        expectation_contract,
        scope_exclusions,
        source_scope,
        logs_root,
        root,
        cargo_authority=cargo_authority,
    )
    base["gate"]["requested"] = bool(args.check)
    base["gate"]["mode"] = "strict_check" if args.check else "evidence_collection_only"
    validate_report_destination()
    _write_json_exclusive(output, base)
    print(
        f"[full-route-census] {'PASS' if base['gate']['passed'] else 'FAIL'}: "
        f"{len(records)}/{len(sources)} inputs; mode={base['gate']['mode']} -> {output}",
        file=sys.stderr,
    )
    return 1 if args.check and not base["gate"]["passed"] else 0


def _count_by(items: Iterable[Mapping[str, Any]], field: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    for item in items:
        value = str(item.get(field, "unknown"))
        counts[value] = counts.get(value, 0) + 1
    return dict(sorted(counts.items()))


def _manifest_digest(manifest: Sequence[Mapping[str, Any]]) -> str:
    payload = json.dumps(
        list(manifest), sort_keys=True, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8")
    return sha256_bytes(payload)


def _manifest_paths_hash(paths: Sequence[str]) -> str:
    payload = "\0".join(sorted(paths)).encode("utf-8")
    return sha256_bytes(payload)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=str(ROOT), help="repository/source root (default: script's repository)")
    parser.add_argument("--mimi", default=os.environ.get("MIMI", str(ROOT / "target/debug/mimi")), help="Mimi CLI binary")
    parser.add_argument("--output", required=True, help="new JSON report path; existing files are never overwritten")
    parser.add_argument("--artifacts", help="new artifact directory (default: <output>.artifacts)")
    parser.add_argument(
        "--expectations",
        help="optional schema-v4 typed expectation manifest with test authority and source hashes",
    )
    parser.add_argument("--timeout", type=float, default=120.0, help="per-input timeout in seconds (default: 120)")
    parser.add_argument(
        "--jobs",
        type=int,
        default=4,
        help="maximum parallel input builds (1-8; default: 4)",
    )
    parser.add_argument("--scratch-root", help="temporary build workspace root (default: <root>/target)")
    parser.add_argument("--inventory-only", action="store_true", help="write the full denominator without invoking Mimi")
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail closed unless every input is successful, fully named, zero-fallback, and default Canonical",
    )
    args = parser.parse_args(argv)
    if args.timeout <= 0:
        parser.error("--timeout must be greater than zero")
    if not 1 <= args.jobs <= 8:
        parser.error("--jobs must be between 1 and 8")
    if args.inventory_only and args.check:
        parser.error("--check cannot be combined with --inventory-only")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        return collect(args)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"[full-route-census] error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
