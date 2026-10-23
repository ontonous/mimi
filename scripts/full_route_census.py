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
without invoking Mimi. A full run uses a bounded worker pool and commits its
journal and progress in manifest order. Add
``--check`` to make the command a fail-closed gate: every denominator entry
must build through the default Canonical route, have a complete named
per-function eligibility/emission roster, and have no legacy fallback. The
census parses the route-specific native emitter roster: canonical MIR builds
report every verified MIR owner and reconcile those names, counts, and graph
digest; resolved-native builds reconcile their named events against dispatch
and emitter totals.

Expectations are a typed, evidence-backed overlay. A mapped input binds its
Mimi source digest to a named test assertion and the digest/location of that
assertion's source file. Missing entries are reported as ``unmapped``; there is
no implicit success expectation. For example:

    {"schema_version": 2, "inputs": {
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
``expected_exit_code``, a non-empty ``expected_diagnostic_codes`` list, and a
non-empty ``required_output_substrings`` list. Those diagnostics must match the
captured output exactly, and the build must leave no output artifact. A valid
``interpreter_only`` entry skips the build and records ``run_status=not_run``.

An expected build rejection's named authority must be an active Rust ``#[test]``
that builds the same repository-relative Mimi path with the default ``build``
command and asserts that command's failure plus every expected diagnostic code
and output substring. The census independently checks the observed command
exit, exact diagnostic-code set, required substrings, and artifact absence.

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

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_VERSION = "0.1.11-full-route-census-v3"
EXPECTATION_SCHEMA_VERSION = 2
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
ACTOR_EXPECTED_STDOUT = {
    ACTOR_FALLBACK_INPUTS[0]: b"15\n",
    ACTOR_FALLBACK_INPUTS[1]: b"42\n101\n",
}
LEGACY_OWNER_NAMES = (
    "CodegenLegacyRemainder",
    "FlowVerifierCompatibility",
    "FfiVerifierCompatibility",
    "DualVerifierCompatibility",
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
MIR_EMIT_SUCCESS_RE = re.compile(r"^info: canonical MIR native emitted owner='(.+)'$")
MIR_EMIT_SUMMARY_RE = re.compile(
    r"^info: canonical MIR native compiled (\d+)/(\d+) function\(s\), "
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
            if name in EXCLUDED_DIRS:
                continue
            if path.is_symlink():
                target_bytes = os.fsencode(os.readlink(path))
                excluded.append({
                    "path": relative_path(path, root),
                    "kind": "directory_symlink_not_traversed",
                    "symlink_target": os.fsdecode(target_bytes),
                    "symlink_target_sha256": sha256_bytes(target_bytes),
                })
            else:
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


def _command_result(command: Sequence[str], *, timeout: float = 8.0) -> dict[str, Any]:
    try:
        proc = subprocess.run(
            list(command), capture_output=True, timeout=timeout, check=False
        )
        return {
            "command": list(command),
            "exit_code": proc.returncode,
            "stdout": _decode(proc.stdout),
            "stderr": _decode(proc.stderr),
        }
    except subprocess.TimeoutExpired as error:
        return {
            "command": list(command),
            "exit_code": None,
            "stdout": _decode(error.output),
            "stderr": _decode(error.stderr),
            "status": "timeout",
        }
    except OSError as error:
        return {
            "command": list(command),
            "exit_code": None,
            "stdout": "",
            "stderr": str(error),
            "status": "spawn_error",
        }


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


def toolchain_manifest(mimi: Path, *, probe_cli: bool = True) -> dict[str, Any]:
    mimi_path = mimi.expanduser().resolve()
    binary: dict[str, Any] = {"path": str(mimi_path), "exists": mimi_path.is_file()}
    if mimi_path.is_file():
        binary["sha256"] = sha256_file(mimi_path)
        binary["bytes"] = mimi_path.stat().st_size
    if probe_cli and mimi_path.is_file():
        binary["version"] = _command_result([str(mimi_path), "--version"])
        build_help = _command_result([str(mimi_path), "build", "--help"])
        stats_help = _command_result([str(mimi_path), "stats", "--help"])
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
            "version": _command_result([str(llvm_config), "--version"]),
        }
        if llvm_config
        else {"status": "not_found", "candidates_checked": [str(p) for p in llvm_candidates]}
    )
    z3_identity = _tool_identity(os.environ.get("Z3", "z3"))
    z3_identity["version"] = (
        _command_result([z3_identity["path"], "--version"])
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
        "rustc": {**_tool_identity("rustc"), "version": _command_result(["rustc", "--version", "--verbose"])},
        "cargo": {**_tool_identity("cargo"), "version": _command_result(["cargo", "--version"])},
        "cc": {**_tool_identity(os.environ.get("CC", "cc")), "version": _command_result([os.environ.get("CC", "cc"), "--version"])},
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

    attributes = _rust_outer_attributes_before(code_text, match.start())
    if attributes is None:
        return None
    normalized = [re.sub(r"\s+", "", attribute) for attribute in attributes]
    if normalized.count("#[test]") != 1:
        return None
    if any(
        re.match(r"#\[(?:ignore)(?:=|\(|\])", attribute)
        or re.match(r"#\[(?:cfg|cfg_attr)\(", attribute)
        for attribute in normalized
    ):
        # Any conditional attribute makes activity dependent on the build
        # environment. Fail closed instead of guessing at the active cfg set.
        return None

    opening_index = match.end() - 1
    end = _find_rust_delimiter_end(code_text, opening_index, "{", "}")
    if end is None:
        return None
    return match.start(), end


def actor_fixture_test_contracts(root: Path) -> dict[str, dict[str, Any]]:
    """Bind each required Actor fixture to an active test's exact helper call."""
    authority_path = "tests/real_world_cli.rs"
    authority_file = root / authority_path
    try:
        authority_bytes = authority_file.read_bytes()
        authority_text = authority_bytes.decode("utf-8")
        authority_digest = sha256_bytes(authority_bytes)
    except (OSError, UnicodeDecodeError):
        authority_text = ""
        authority_digest = None
    bindings = (
        (
            ACTOR_FALLBACK_INPUTS[0],
            "canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback",
        ),
        (
            ACTOR_FALLBACK_INPUTS[1],
            "canonical_actor_shadow_original_uses_default_and_explicit_cli_without_fallback",
        ),
    )
    contracts: dict[str, dict[str, Any]] = {}
    for source_path, test_id in bindings:
        expected_stdout = ACTOR_EXPECTED_STDOUT[source_path]
        span = _rust_test_function_span(authority_text, test_id)
        assertion: dict[str, Any] | None = None
        if span is not None and authority_digest is not None:
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
                line = authority_text.count("\n", 0, matching_call) + 1
                assertion = {
                    "path": authority_path,
                    "line": line,
                    "sha256": authority_digest,
                }
        contracts[source_path] = {
            "test_id": test_id,
            "authority_valid": assertion is not None,
            "authority": {
                "test_id": test_id,
                "assertion": assertion,
            } if assertion is not None else None,
            "expected_stdout": _raw_capture(expected_stdout),
        }
    return contracts


def _rust_string_literals(text: str) -> list[str]:
    return [match.group(1) for match in re.finditer(r'"((?:\\.|[^"\\])*)"', text)]


def _expected_rejection_test_authority_is_valid(
    *,
    test_id: Any,
    source_path: str,
    target_kind: str,
    source_text: str,
    assertion_line: Any,
    expected_diagnostic_codes: Any,
    required_output_substrings: Any,
) -> bool:
    """Require an active test to invoke the exact input's default build and assert its refusal."""
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
    ):
        return False
    span = _rust_test_function_span(source_text, test_id)
    if span is None:
        return False
    function_start, function_end = span
    function_text = source_text[function_start:function_end]

    source_parts = Path(source_path).parts
    matching_fixture_variables: set[str] = set()
    for variable_match in re.finditer(
        r"\blet\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;]+);", function_text, re.S
    ):
        initializer = variable_match.group(2)
        joined_parts = re.findall(
            r"\.join\(\s*\"((?:\\.|[^\"\\])*)\"\s*\)", initializer
        )
        if any(
            tuple(joined_parts[index : index + len(source_parts)]) == tuple(source_parts)
            for index in range(max(0, len(joined_parts) - len(source_parts) + 1))
        ):
            matching_fixture_variables.add(variable_match.group(1))
    if not matching_fixture_variables:
        return False

    command_bindings: list[str] = []
    for variable_match in re.finditer(
        r"\blet\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;]+);", function_text, re.S
    ):
        initializer = variable_match.group(2)
        command_match = re.search(
            r"Command::new\s*\(\s*mimi_bin\s*\(\s*\)\s*\)"
            r"(?P<chain>[\s\S]*?)\.output\s*\(\s*\)",
            initializer,
        )
        if command_match is None:
            continue
        chain = command_match.group("chain")
        arguments = re.findall(
            r"\.arg\(\s*(&?[A-Za-z_][A-Za-z0-9_]*|\"(?:\\.|[^\"\\])*\")\s*\)",
            chain,
        )
        shared_argument = ['"--shared"'] if target_kind == "shared_library_probe" else []
        for fixture_variable in matching_fixture_variables:
            expected_prefix = ['"build"', *shared_argument, f"&{fixture_variable}", '"-o"']
            if (
                len(arguments) == len(expected_prefix) + 1
                and arguments[:-1] == expected_prefix
                and not arguments[-1].startswith("&")
            ):
                command_bindings.append(variable_match.group(1))
    if not command_bindings:
        return False

    expected_literals = [
        *[str(code).upper() for code in expected_diagnostic_codes],
        *[str(item) for item in required_output_substrings],
    ]
    for command_variable in command_bindings:
        if not re.search(
            rf"assert!\s*\(\s*!\s*{re.escape(command_variable)}\.status\.success\s*\(\s*\)",
            function_text,
        ):
            continue
        stderr_binding = re.search(
            rf"\blet\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*String::from_utf8_lossy\s*"
            rf"\(\s*&{re.escape(command_variable)}\.stderr\s*\)",
            function_text,
        )
        if stderr_binding is None:
            continue
        stderr_variable = stderr_binding.group(1)
        diagnostic_assertions: list[tuple[int, int, str, set[str]]] = []
        for assertion_match in re.finditer(r"\bassert!\s*\(", function_text):
            open_index = assertion_match.end() - 1
            assertion_end = _find_rust_delimiter_end(
                function_text, open_index, "(", ")"
            )
            if assertion_end is None:
                continue
            assertion_text = function_text[assertion_match.start() : assertion_end]
            if not re.search(
                rf"\b{re.escape(stderr_variable)}\s*\.\s*contains\s*\(",
                assertion_text,
            ):
                continue
            literals = set(_rust_string_literals(assertion_text))
            if any(literal in literals for literal in expected_literals):
                start_line = source_text.count("\n", 0, function_start + assertion_match.start()) + 1
                end_line = source_text.count("\n", 0, function_start + assertion_end) + 1
                diagnostic_assertions.append((start_line, end_line, assertion_text, literals))

        asserted_literals = {
            literal
            for _start, _end, _assertion, literals in diagnostic_assertions
            for literal in literals
        }
        if not set(expected_literals).issubset(asserted_literals):
            continue
        if not any(start <= assertion_line <= end for start, end, _assertion, _literals in diagnostic_assertions):
            continue
        return True
    return False


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
        item["authority_status"] = "not_applicable"
        item["authority_valid"] = False
        item["test_execution_valid"] = None
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
                        item["test_execution_valid"] = (
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
                            )
                        )
                    regular_source_file = assertion_file.is_file() and not assertion_file.is_symlink()
                except (OSError, UnicodeDecodeError, ValueError, TypeError):
                    assertion_hash_matches = False
                    test_id_present = False
                    assertion_line_valid = False
                    regular_source_file = False
                    if item.get("class") == "expected_build_rejection":
                        item["test_execution_valid"] = False
                authority_valid = (
                    regular_source_file
                    and assertion_hash_matches
                    and test_id_present
                    and assertion_line_valid
                    and (
                        item.get("class") != "expected_build_rejection"
                        or item.get("test_execution_valid") is True
                    )
                )
                item["authority_status"] = (
                    "valid"
                    if authority_valid
                    else (
                        "rejection_test_contract_mismatch"
                        if (
                            item.get("class") == "expected_build_rejection"
                            and regular_source_file
                            and assertion_hash_matches
                            and test_id_present
                            and assertion_line_valid
                        )
                        else "source_location_or_hash_mismatch"
                    )
                )
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
    for line in captured.splitlines():
        match = ROUTE_RE.search(_strip_ansi(line))
        if match:
            raw = match.group(1)
            observations.append({
                "disposition": "canonical" if raw == "canonical" else "compatibility",
                "raw_disposition": raw,
                "reason": match.group(2),
                "mir_digest": match.group(3),
                "line": line,
            })
            continue
        plain_line = _strip_ansi(line)
        rejected = REJECT_RE.search(plain_line) or EXPLICIT_REJECT_RE.search(plain_line)
        if rejected:
            observations.append({
                "disposition": "rejected",
                "raw_disposition": "rejected",
                "reason": rejected.group(1).strip() or "canonical route rejected",
                "mir_digest": None,
                "line": line,
            })
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
            "reason": observations[0]["reason"] if len(observations) == 1 else None,
            "mir_digest": observations[0]["mir_digest"] if len(observations) == 1 else None,
        }
    if observations:
        return {
            "disposition": "unobserved",
            "classification_status": "conflicting_observations",
            "observations": observations,
            "reason": "conflicting whole-program route identities or dispositions were emitted",
            "mir_digest": None,
        }
    if run_status not in {"completed"}:
        return {
            "disposition": "unobserved",
            "classification_status": run_status,
            "observations": [],
            "reason": "route selection could not be observed because process did not complete",
            "mir_digest": None,
        }
    if exit_code not in (None, 0) and DIAGNOSTIC_RE.search(diagnostic_text):
        return {
            "disposition": "rejected",
            "classification_status": "inferred_pre_route_rejection",
            "observations": [],
            "reason": "compiler emitted a failure diagnostic before any route telemetry",
            "mir_digest": None,
        }
    return {
        "disposition": "unobserved",
        "classification_status": "missing_route_telemetry",
        "observations": [],
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
    events: list[dict[str, Any]] = []
    summaries: list[dict[str, Any]] = []
    for line in (stderr + "\n" + stdout).splitlines():
        match_line = _strip_ansi(line)
        match = MIR_EMIT_SUCCESS_RE.match(match_line)
        if match:
            events.append({
                "function": match.group(1),
                "owner": match.group(1),
                "eligibility": "canonical_mir_owner",
                "emission": "canonical_mir_native",
                "fallback": False,
                "status": "emission_success",
                "source_line": line,
            })
            continue
        match = MIR_EMIT_SUMMARY_RE.match(match_line)
        if match:
            summaries.append({
                "compiled": int(match.group(1)),
                "total_mir_functions": int(match.group(2)),
                "fell_back": int(match.group(3)),
                "mir_digest": match.group(4),
            })

    summary = summaries[0] if len(summaries) == 1 else None
    owners = [event["owner"] for event in events]
    duplicate_owners = sorted({owner for owner in owners if owners.count(owner) > 1})
    total = summary["total_mir_functions"] if summary else None
    compiled = summary["compiled"] if summary else None
    fell_back = summary["fell_back"] if summary else None
    route_digest = route.get("mir_digest")
    summary_digest = summary["mir_digest"] if summary else None
    digest_matches_route = (
        isinstance(route_digest, str)
        and bool(re.fullmatch(r"[0-9a-f]{64}", route_digest))
        and summary_digest == route_digest
    )
    named_count_matches_total = total == len(owners) if total is not None else None
    summary_counts_reconcile = (
        compiled == len(owners) and compiled + fell_back == total
        if summary is not None
        else None
    )
    no_fallback = fell_back == 0 if fell_back is not None else None
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
        "mir_summary_is_unique": len(summaries) == 1,
        "mir_owner_events_are_unique": not duplicate_owners,
        "mir_named_owner_count_matches_total": named_count_matches_total,
        "mir_emitter_counts_reconcile": summary_counts_reconcile,
        "mir_graph_digest_matches_route": digest_matches_route,
        "mir_route_has_no_fallback": no_fallback,
        "resolved_dispatch_stats_absent": stats_status == "not_emitted",
    }
    roster_complete = all(value is True for value in consistency.values())
    missing_owner_count = max(0, total - len(set(owners))) if total is not None else None
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
    fallback_count = fell_back if fell_back is not None else 0
    return {
        "status": "complete_named" if roster_complete else (
            "partial" if events or summaries or stats_status != "not_emitted" else "unavailable"
        ),
        "source": "canonical MIR native owner events plus post-module-verification summary",
        "dispatch_stats": dict(stats_payload) if isinstance(stats_payload, Mapping) else None,
        "dispatch_stats_status": stats_status,
        "counts": {
            "total_functions": total,
            "compiled_canonical_mir": compiled,
            "canonical_mir_fallback": fell_back,
            "named_emission_successes": len(events),
            "unique_named_functions": len(set(owners)),
            "unattributed_function_count": missing_owner_count,
            "fallback_total_observed_or_counted": fallback_count,
        },
        "functions": events,
        "unattributed_function_slots": anonymous_slots,
        "emitter_summary": summary,
        "route_mir_digest": route_digest,
        "consistency": consistency,
        "duplicate_function_names": duplicate_owners,
        "named_roster_complete": roster_complete,
        "has_fallback": fallback_count > 0 or no_fallback is False,
        "limitation": None if roster_complete else (
            "Canonical MIR owner events and their summary are absent, duplicated, or inconsistent "
            "with the MIR graph count/digest or route identity; resolved dispatch stats do not "
            "describe this emitter. --check fails closed."
        ),
    }


def _function_observations(
    stdout: str,
    stderr: str,
    stats: Mapping[str, Any] | None,
    route: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    if (
        route is not None
        and route.get("disposition") == "canonical"
    ):
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
            expectation.get("source_hash_status") == "matched"
            and expectation.get("authority_valid") is True
            and (
                expectation_class != "expected_build_rejection"
                or expectation.get("test_execution_valid") is True
            )
        )
    )
    decoded_output = _decode(captured_output)
    diagnostic_output = _strip_ansi(decoded_output)
    actual_diagnostic_codes = _diagnostic_codes(decoded_output)
    outcome_match = False
    expected_outcome: dict[str, Any]
    diagnostic_contract: dict[str, Any] | None = None
    executable_contract: dict[str, Any] | None = None
    if expectation_class == "required_compile":
        expected_outcome = {"build": "completed", "exit_code": 0, "route": "canonical"}
        outcome_match = (
            run_status == "completed"
            and exit_code == 0
            and route.get("disposition") == "canonical"
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
            "route": "rejected",
            "diagnostic_contract": diagnostic_contract,
            "output_artifact": "absent",
        }
        outcome_match = (
            run_status == "completed"
            and expected_exit_code_valid
            and exit_code == expected_code
            and route.get("disposition") == "rejected"
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
        "test_execution_valid": expectation.get("test_execution_valid"),
        "run_status": run_status,
        "actual_exit_code": exit_code,
        "actual_route": route.get("disposition"),
        "actual_failure_class": failure.get("class"),
        "actual_diagnostic_codes": actual_diagnostic_codes,
        "diagnostic_contract": diagnostic_contract,
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
) -> dict[str, Any]:
    by_path = {str(record.get("path")): record for record in records}
    manifest_by_path = {str(record.get("path")): record for record in manifest}
    manifest_paths = set(manifest_by_path)
    cases: list[dict[str, Any]] = []
    for path in ACTOR_FALLBACK_INPUTS:
        record = by_path.get(path)
        source = manifest_by_path.get(path)
        contract = (actor_contracts or {}).get(path, {})
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
        "missing_cases": missing,
        "source": "B-01/A-05 explicitly retain both former nested capture/shadow compatibility cases in the route denominator",
    }


def _parse_legacy_owner_audit(stdout: str) -> dict[str, Any]:
    owners: dict[str, dict[str, Any]] = {}
    for line in stdout.splitlines():
        match = re.match(r"owner=([^ ]+) production_accessor_call_sites=(\d+)$", line)
        if match:
            owners.setdefault(match.group(1), {})["production_accessor_call_sites"] = int(match.group(2))
            continue
        match = re.match(
            r"owner=([^ ]+) production_access_status=([^ ]+) dependency_class=([^ ]+) "
            r"owner_deletion_ready=([01]) owner_deletion_blocker=(.*)",
            line,
        )
        if match:
            item = owners.setdefault(match.group(1), {})
            item.update({
                "production_access_status": match.group(2),
                "dependency_class": match.group(3),
                "owner_deletion_ready": match.group(4) == "1",
                "owner_deletion_blocker": match.group(5),
            })
            continue
        match = re.match(r"owner=([^ ]+) owner_deletion_condition_digest=([0-9a-f]{64})$", line)
        if match:
            owners.setdefault(match.group(1), {})["deletion_condition_sha256"] = match.group(2)
            continue
        match = re.match(r"owner_deletion_condition=(.*)$", line)
        if match:
            # The audit prints one condition directly after each owner digest.
            pending = next(
                (name for name in LEGACY_OWNER_NAMES if name in owners and "deletion_condition" not in owners[name]),
                None,
            )
            if pending is not None:
                owners[pending]["deletion_condition"] = match.group(1)
    parsed = [
        {"name": name, **owners.get(name, {})}
        for name in LEGACY_OWNER_NAMES
    ]
    actual_condition_set = "".join(
        f"{owner['name']}={owner.get('deletion_condition_sha256', '')}\n"
        for owner in parsed
    )
    reported_set_digest = _parse_assignment(stdout, "owner_deletion_condition_set_digest")
    condition_digests_match = all(
        owner.get("deletion_condition_sha256") == sha256_bytes(
            str(owner.get("deletion_condition", "")).encode("utf-8")
        )
        for owner in parsed
    )
    condition_set_digest_matches = (
        reported_set_digest == sha256_bytes(actual_condition_set.encode("utf-8"))
    )
    complete = all(
        isinstance(owner.get("production_accessor_call_sites"), int)
        and owner.get("production_access_status") in {"sealed", "open"}
        and isinstance(owner.get("dependency_class"), str)
        and isinstance(owner.get("owner_deletion_ready"), bool)
        and isinstance(owner.get("owner_deletion_blocker"), str)
        and isinstance(owner.get("deletion_condition"), str)
        and bool(owner.get("deletion_condition"))
        and isinstance(owner.get("deletion_condition_sha256"), str)
        for owner in parsed
    ) and condition_digests_match and condition_set_digest_matches
    return {
        "owners": parsed,
        "owner_count_reported": _parse_int_assignment(stdout, "owner_count"),
        "owner_deletion_condition_set_sha256": reported_set_digest,
        "deletion_condition_digests_match_text": condition_digests_match,
        "deletion_condition_set_digest_matches": condition_set_digest_matches,
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
    expected = set(LEGACY_OWNER_NAMES)
    actual = {str(owner["name"]) for owner in parsed["owners"] if owner.get("production_accessor_call_sites") is not None}
    accepted = (
        run_status == "completed"
        and exit_code == 0
        and parsed["owner_count_reported"] == len(LEGACY_OWNER_NAMES)
        and actual == expected
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
        or expected is None
    ):
        return False
    path = logs_root / name
    try:
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode):
            return False
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
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
    except OSError:
        return False
    return actual == expected


def _write_json_exclusive(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    data = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False).encode("utf-8") + b"\n"
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


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
) -> dict[str, Any]:
    def nested(record: Mapping[str, Any], key: str) -> Mapping[str, Any]:
        value = record.get(key)
        return value if isinstance(value, Mapping) else {}

    def raw_capture_is_complete(value: Any) -> bool:
        if not isinstance(value, Mapping):
            return False
        encoded = value.get("base64")
        expected_hash = value.get("sha256")
        expected_bytes = value.get("bytes")
        if not isinstance(encoded, str) or not isinstance(expected_hash, str):
            return False
        if not isinstance(value.get("path"), str):
            return False
        try:
            raw = base64.b64decode(encoded, validate=True)
        except (ValueError, base64.binascii.Error):
            return False
        return (
            sha256_bytes(raw) == expected_hash
            and isinstance(expected_bytes, int)
            and not isinstance(expected_bytes, bool)
            and len(raw) == expected_bytes
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
    }
    noncanonical_routes = [
        str(record.get("path"))
        for record in records
        if nested(record, "route").get("disposition") != "canonical"
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
        if not nested(record, "per_function").get("named_roster_complete", False)
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
    owner_captures = [
        legacy_owner_audit.get("stdout_raw"),
        legacy_owner_audit.get("stderr_raw"),
    ]
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
        or nested(record, "expectation").get("authority_valid") is True
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
    canonical_success_inputs = sorted(
        str(record.get("path"))
        for record in records
        if nested(record, "expectation").get("class") == "required_compile"
        and nested(record, "expectation").get("matched") is True
        and nested(record, "expectation").get("source_hash_status") == "matched"
        and nested(record, "expectation").get("authority_valid") is True
        and record.get("run_status") == "completed"
        and record.get("exit_code") == 0
        and nested(record, "route").get("disposition") == "canonical"
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
    not_successful = [
        str(record.get("path"))
        for record in records
        if record.get("run_status") != "completed" or record.get("exit_code") != 0
    ]
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
    owner_rows_complete = (
        owner_names == set(LEGACY_OWNER_NAMES)
        and len(owner_rows) == len(LEGACY_OWNER_NAMES)
        and legacy_owner_audit.get("owner_count_reported") == len(LEGACY_OWNER_NAMES)
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
        "actor_fallback_cases_are_canonical_without_fallback": actor_no_fallback,
        "all_four_legacy_owners_are_audited": (
            bool(legacy_owner_audit.get("accepted_as_complete_inventory")) and owner_rows_complete
        ),
        "no_missing_tracked_inputs": not missing_manifest_inputs,
        "no_unfollowed_mimi_symlinks": not excluded_symlinks,
        "no_tracked_mimi_paths_through_symlinked_parents": not tracked_symlink_parent_blockers,
        "all_whole_program_routes_are_canonical": not noncanonical_routes,
        "no_legacy_or_emission_fallback": not fallback_inputs,
        "all_per_function_eligibility_is_named_and_reconciled": not per_function_incomplete,
        "all_builds_succeeded": not not_successful,
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
        "legacy_owner_rows_complete": owner_rows_complete,
        "unfollowed_mimi_symlinks": [item["path"] for item in excluded_symlinks],
        "tracked_symlink_parent_blockers": tracked_symlink_parent_blockers,
        "noncanonical_route_inputs": noncanonical_routes,
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
        "legacy_owner_names_requiring_individual_status": list(LEGACY_OWNER_NAMES),
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
    try:
        requested_output_lexical.relative_to(root)
    except ValueError:
        lexical_output_inside_root = False
    else:
        lexical_output_inside_root = True
    try:
        output.relative_to(root)
    except ValueError:
        resolved_output_inside_root = False
    else:
        resolved_output_inside_root = True
    if (
        lexical_output_inside_root and requested_output_lexical.suffix == ".mimi"
    ) or (resolved_output_inside_root and output.suffix == ".mimi"):
        raise ValueError(
            f"refusing to place the route census report over an in-scope Mimi path: {output}"
        )
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
        "toolchain": toolchain_manifest(mimi, probe_cli=not args.inventory_only),
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
            "route_classes": ["canonical", "compatibility", "rejected", "unobserved"],
            "per_function_requirement": "route-specific named emitter disposition for every canonical executable owner; canonical MIR events reconcile with the MIR function count and route digest after module verification, while resolved-native events reconcile with MIMI_STAT and their emitter summary",
            "failure_classes": ["capability", "library_target", "environment", "unknown", "none"],
            "build_skip_rule": "a validated explicit interpreter_only expectation skips invocation and records a reason; this row is never canonical",
            "strict_check": "requires exact active candidate inventory, exact-once source-scope accounting, one route classification per candidate, valid expectation evidence, a canonical route for every candidate, no compatibility/function fallback, complete named per-function rosters for canonical builds, classified failures, and stable source fingerprint; matched intentional rejections are counted separately and remain noncanonical blockers",
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
            sources, [], actor_contracts
        )
        base["legacy_owner_audit"] = {
            "status": "not_run_inventory_only",
            "owners": [{"name": name, "status": "not_assessed"} for name in LEGACY_OWNER_NAMES],
        }
        base["candidate_classification_counts"] = _count_by(
            [{"classification": row["candidate_classification"]} for row in base["records"]],
            "classification",
        )
        base["gate"] = {"passed": None, "status": "not_evaluated_inventory_only", "strict_gate_available": False}
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
        sources, records, actor_contracts
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
    )
    base["gate"]["requested"] = bool(args.check)
    base["gate"]["mode"] = "strict_check" if args.check else "evidence_collection_only"
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
        help="optional schema-v2 typed expectation manifest with test authority and source hashes",
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
