#!/usr/bin/env python3
"""Source-bound release evidence for the V0.2 gates.

This module deliberately stays independent from Mimi's semantic test harness.  It
records observations made by real commands and refuses to infer success from a
file merely existing.  In particular, an exit code of zero without a route,
backend, verifier, or native-memory observation is ``environment_unverified``.

The hyphenated :mod:`release-evidence.py` file is the command line entry point;
the functions in this module are intentionally small so CI and isolated tests
can use the same checks without importing the compiler.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any

SCHEMA_VERSION = "v0.2-release-evidence-v2"
SCRIPT = Path(__file__).resolve()
DEFAULT_INPUT_DIRS = ("src", "tests", "stdlib", "examples", "projects", "fixtures", "benches")
DEFAULT_ROOT_INPUTS = (
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/config.toml",
)
EXCLUDED_DIRS = {".git", "target", "node_modules", "__pycache__"}
ENV_KEYS = {
    "LLVM_SYS_181_PREFIX",
    "LLVM_CONFIG_PATH",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "CARGO_TARGET_DIR",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUST_TEST_THREADS",
    "RUST_TEST_NOCAPTURE",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
    "LD",
    "LIBRARY_PATH",
    "LD_LIBRARY_PATH",
    "PKG_CONFIG_PATH",
    "ASAN_OPTIONS",
    "LSAN_OPTIONS",
    "TSAN_OPTIONS",
    "UBSAN_OPTIONS",
    "CARGO_NET_OFFLINE",
    "MIMI_OFFLINE",
    "PATH",
}

STATUS_LABELS = {
    "passed": "已通过",
    "explicit_reject": "明确拒绝",
    "environment_unverified": "环境未验证",
    "unsupported": "不支持",
    "unexplained_failure": "未解释失败",
    "not_run": "未运行",
}
TERMINAL_STATUSES = frozenset(STATUS_LABELS)
PASS_STATUS = "passed"

# The compiler has emitted a few generations of these diagnostics.  Keep the
# parser liberal, while requiring at least one concrete marker before accepting
# a gate.  A log with no marker is never considered proof.
ROUTE_RE = re.compile(
    r"(?:canonical\s+route\s+disposition|route\s+disposition)\s*:\s*"
    r"(?P<disposition>canonical|legacy|rejected|compatibility)"
    r"(?:\s*\((?P<reason>[^)]+)\))?"
    r"(?:\s+mir_digest=(?P<digest>[0-9a-fA-F]{64}))?",
    re.IGNORECASE,
)
ROUTE_JSON_RE = re.compile(
    r"[\"'](?:route_disposition|route)[\"']\s*:\s*[\"']"
    r"(?P<disposition>canonical|legacy|rejected|compatibility)[\"']",
    re.IGNORECASE,
)
BACKEND_RE = re.compile(
    r"(?:backend|consumer|executor|execution\s+backend)\s*(?:=|:|->)\s*"
    r"(?P<backend>reference|ref|vm|bytecode|native|llvm|verifier|z3|host[-_ ]?c)\b",
    re.IGNORECASE,
)
BACKEND_JSON_RE = re.compile(
    r"[\"'](?:backend|consumer|executor)[\"']\s*:\s*[\"']"
    r"(?P<backend>[^\"']+)[\"']",
    re.IGNORECASE,
)
VALIDATOR_RE = re.compile(
    r"(?:verif(?:ier|ication)|validator|proof)\s*(?:status|result)?\s*"
    r"(?:=|:|->)\s*"
    r"(?P<status>Proven|Disproven|NotInTrustedSubset|SolverUnknown|Timeout|"
    r"InfrastructureError|RuntimeOnlyContract|NoObligations|Unknown|passed|failed)\b",
    re.IGNORECASE,
)
VALIDATOR_JSON_RE = re.compile(
    r"[\"'](?:verifier_status|validator_status|proof_status|verification_status)[\"']\s*:\s*"
    r"[\"'](?P<status>[^\"']+)[\"']",
    re.IGNORECASE,
)
ABI_RE = re.compile(
    r"(?:abi(?:_digest|\s+digest)?|interface\s+digest)\s*(?:=|:|->)\s*"
    r"(?P<digest>[0-9a-fA-F]{64})",
    re.IGNORECASE,
)
MEMORY_RSS_RE = re.compile(
    r"Maximum resident set size\s*\(kbytes\):\s*(?P<rss>\d+)", re.IGNORECASE
)
MEMORY_ALLOC_RE = re.compile(
    r"(?:alloc(?:ations?)?|heap allocations?)\s*(?:=|:)\s*(?P<count>\d+)", re.IGNORECASE
)
VALGRIND_ERROR_RE = re.compile(
    r"(?:ERROR SUMMARY:\s*(?P<errors>\d+) errors?|"
    r"definitely lost:\s*(?P<lost>[0-9,]+) bytes?|"
    r"indirectly lost:\s*(?P<indirect>[0-9,]+) bytes?)",
    re.IGNORECASE,
)
SANITIZER_ERROR_RE = re.compile(
    r"(?:AddressSanitizer|LeakSanitizer|ThreadSanitizer|UndefinedBehaviorSanitizer)"
    r"[^\n]*(?:ERROR|error|failed|detected)",
    re.IGNORECASE,
)
NETWORK_RE = re.compile(
    r"(?:updating\s+crates\.io|downloading|downloaded|fetching|failed to download|"
    r"network request|could not resolve host|connection refused|proxy)",
    re.IGNORECASE,
)
CARGO_FINISHED_RE = re.compile(
    r'"reason"\s*:\s*"build-finished"\s*,\s*"success"\s*:\s*true', re.IGNORECASE
)
SOURCE_IDENTITY_RE = re.compile(
    r"(?:source[_ ]identity|evidence[_ ]source[_ ]identity)\s*(?:=|:|->)\s*"
    r"(?P<identity>[0-9a-fA-F]{64})",
    re.IGNORECASE,
)
COMMIT_RE = re.compile(
    r"(?:source[_ ]commit|commit|git[_ ]commit)\s*(?:=|:|->)\s*"
    r"(?P<commit>[0-9a-fA-F]{7,64})\b",
    re.IGNORECASE,
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def canonical_hash(value: Any) -> str:
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    return sha256_bytes(payload)


def _immutable_write(path: Path, value: Any) -> Path:
    """Write one JSON record exactly once.

    Evidence is append-only by file name.  Refusing replacement is intentional:
    a later retry must retain the first failure rather than turning it green.
    """

    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(value, dict):
        body = {key: item for key, item in value.items() if key != "record_hash"}
        value["record_hash"] = canonical_hash(body)
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    return path


def _record_hash_valid(record: Mapping[str, Any]) -> bool:
    expected = record.get("record_hash")
    body = {key: item for key, item in record.items() if key != "record_hash"}
    return isinstance(expected, str) and expected == canonical_hash(body)


def _failure_dir(evidence: Path) -> Path:
    path = evidence / "failures"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _write_failure(evidence: Path, evidence_id: str, reason: str, **details: Any) -> Path:
    """Retain a failure under a unique name, even when an earlier attempt exists."""

    base = re.sub(r"[^A-Za-z0-9_.-]+", "_", evidence_id) or "evidence"
    stamp = f"{time.time_ns()}"
    path = _failure_dir(evidence) / f"{base}-{stamp}.json"
    payload = {
        "schema_version": SCHEMA_VERSION,
        "evidence_id": evidence_id,
        "accepted": False,
        "status": "unexplained_failure",
        "result_class": STATUS_LABELS["unexplained_failure"],
        "reason": reason,
        "recorded_at_unix": time.time(),
        **details,
    }
    _immutable_write(path, payload)
    return path


def _run_git(root: Path, *args: str) -> tuple[int, bytes, bytes]:
    try:
        process = subprocess.run(
            ["git", *args], cwd=root, capture_output=True, check=False
        )
    except OSError as error:
        return 127, b"", str(error).encode()
    return process.returncode, process.stdout, process.stderr


def _git_output(root: Path, *args: str) -> str | None:
    code, stdout, _ = _run_git(root, *args)
    if code != 0:
        return None
    return stdout.decode(errors="replace").strip()


def _tracked_files(root: Path) -> set[str]:
    code, stdout, _ = _run_git(root, "ls-files", "-z")
    if code != 0:
        return set()
    return {item.decode(errors="surrogateescape") for item in stdout.split(b"\0") if item}


def _relative_file(root: Path, path: Path) -> str:
    try:
        return str(path.resolve().relative_to(root.resolve()))
    except ValueError:
        return str(path.resolve())


def _input_paths(root: Path, extras: Iterable[str]) -> list[Path]:
    tracked = _tracked_files(root)
    names: set[str] = set(tracked)
    requested = list(DEFAULT_INPUT_DIRS) + list(DEFAULT_ROOT_INPUTS) + list(extras)
    for relative in requested:
        candidate = root / relative
        if candidate.is_dir():
            for path in candidate.rglob("*"):
                if not path.is_file() and not path.is_symlink():
                    continue
                if any(part in EXCLUDED_DIRS or part.startswith(".tmp") for part in path.relative_to(root).parts):
                    continue
                names.add(str(path.relative_to(root)))
        elif candidate.is_file() or candidate.is_symlink():
            names.add(str(candidate.relative_to(root)))
        elif relative in extras:
            # Keep an explicit missing input in the snapshot.  The resulting
            # manifest is visibly unverified instead of disappearing behind a
            # path-existence check.
            names.add(relative)
    result: list[Path] = []
    for relative in sorted(names):
        path = root / relative
        if path.is_file() or path.is_symlink() or str(path.relative_to(root)) in extras:
            result.append(path)
    return result


def _file_record(root: Path, path: Path) -> dict[str, Any]:
    relative = _relative_file(root, path)
    record: dict[str, Any] = {"path": relative}
    if path.is_symlink():
        record["kind"] = "symlink"
        record["target"] = os.readlink(path)
        resolved = path.resolve()
        record["target_exists"] = resolved.exists()
        if resolved.is_file():
            record["sha256"] = sha256_file(resolved)
            record["bytes"] = resolved.stat().st_size
    elif path.is_file():
        record.update({"kind": "file", "sha256": sha256_file(path), "bytes": path.stat().st_size,
                       "mode": path.stat().st_mode & 0o777})
    else:
        record.update({"kind": "missing", "status": "missing"})
    return record


def observed_environment() -> dict[str, str]:
    return {
        key: value
        for key, value in os.environ.items()
        if key in ENV_KEYS or key.startswith(("CARGO_PROFILE_", "CARGO_TARGET_", "MIMI_"))
    }


def _tool_probe(name: str, command: Sequence[str]) -> dict[str, Any]:
    executable = shutil.which(command[0])
    if not executable:
        return {"name": name, "command": list(command), "status": "missing"}
    try:
        process = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    except OSError as error:
        return {"name": name, "command": list(command), "status": "error", "error": str(error)}
    result: dict[str, Any] = {
        "name": name,
        "command": list(command),
        "status": "observed" if process.returncode == 0 else "error",
        "exit_code": process.returncode,
        "version_output": process.stdout.decode(errors="replace"),
        "executable": str(Path(executable).resolve()),
    }
    if Path(executable).is_file():
        result["executable_sha256"] = sha256_file(Path(executable))
    return result


def toolchain_snapshot() -> dict[str, Any]:
    llvm_prefix = os.environ.get("LLVM_SYS_181_PREFIX")
    llvm = str(Path(llvm_prefix) / "bin/llvm-config") if llvm_prefix else "llvm-config-18"
    probes = [
        _tool_probe("rustc", [os.environ.get("RUSTC", "rustc"), "--version", "--verbose"]),
        _tool_probe("cargo", ["cargo", "--version"]),
        _tool_probe("llvm", [llvm, "--version"]),
        _tool_probe("z3", ["z3", "--version"]),
        _tool_probe("cc", [os.environ.get("CC", "cc"), "--version"]),
    ]
    snapshot = {"platform": {"sysname": os.uname().sysname, "machine": os.uname().machine},
                "tools": probes, "environment": observed_environment()}
    snapshot["identity"] = canonical_hash(snapshot)
    return snapshot


def source_snapshot(root: Path | str, extras: Iterable[str] = ()) -> dict[str, Any]:
    """Capture source and toolchain identity used by every evidence entry."""

    root = Path(root).resolve()
    commit = _git_output(root, "rev-parse", "HEAD")
    branch = _git_output(root, "branch", "--show-current") or ""
    diff_code, diff, diff_err = _run_git(root, "diff", "HEAD", "--binary")
    tracked_diff_hash = sha256_bytes(diff) if diff_code == 0 else None
    files = [_file_record(root, path) for path in _input_paths(root, extras)]
    source_tree_hash = canonical_hash(files)
    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "root": str(root),
        "commit": commit,
        "branch": branch,
        "tracked_diff_hash": tracked_diff_hash,
        "source_files": files,
        "source_tree_hash": source_tree_hash,
        "collector_sha256": sha256_file(SCRIPT),
        "explicit_inputs": list(extras),
        "git_status": "observed" if commit and diff_code == 0 else "unavailable",
        "git_error": None if commit and diff_code == 0 else diff_err.decode(errors="replace"),
        "toolchain": toolchain_snapshot(),
        "limitations": [
            "An identity proves provenance only; it does not prove semantic correctness.",
            "External runtime/FFI libraries must be supplied as hashed artifacts.",
        ],
    }
    result["source_identity"] = canonical_hash({
        "commit": result["commit"],
        "tracked_diff_hash": result["tracked_diff_hash"],
        "source_tree_hash": result["source_tree_hash"],
        "collector_sha256": result["collector_sha256"],
    })
    result["identity"] = canonical_hash({
        "source_identity": result["source_identity"],
        "toolchain_identity": result["toolchain"]["identity"],
    })
    return result


def source_identity_changed(before: Mapping[str, Any], after: Mapping[str, Any]) -> bool:
    return before.get("source_identity") != after.get("source_identity")


def _validate_source_manifest(baseline: Mapping[str, Any]) -> None:
    if baseline.get("schema_version") != SCHEMA_VERSION or not _record_hash_valid(baseline):
        raise ValueError("source manifest schema or content hash is invalid")
    toolchain = baseline.get("toolchain")
    if not isinstance(toolchain, dict) or not isinstance(toolchain.get("identity"), str):
        raise ValueError("source manifest has no toolchain identity")
    expected_toolchain = canonical_hash({key: value for key, value in toolchain.items() if key != "identity"})
    if toolchain.get("identity") != expected_toolchain:
        raise ValueError("source manifest toolchain identity does not match its contents")
    expected_source = canonical_hash({
        "commit": baseline.get("commit"),
        "tracked_diff_hash": baseline.get("tracked_diff_hash"),
        "source_tree_hash": baseline.get("source_tree_hash"),
        "collector_sha256": baseline.get("collector_sha256"),
    })
    if baseline.get("source_identity") != expected_source:
        raise ValueError("source manifest source identity does not match its contents")


def _normalise_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    return value.decode(errors="replace") if isinstance(value, bytes) else value


def _json_key_values(text: str) -> Iterable[tuple[str, Any]]:
    """Yield nested JSON key/value pairs from line-oriented compiler receipts."""

    def walk(value: Any) -> Iterable[tuple[str, Any]]:
        if isinstance(value, dict):
            for key, item in value.items():
                yield str(key), item
                yield from walk(item)
        elif isinstance(value, list):
            for item in value:
                yield from walk(item)

    for line in text.splitlines():
        try:
            value = json.loads(line)
        except (TypeError, json.JSONDecodeError):
            continue
        yield from walk(value)


def parse_route(text: str | bytes) -> dict[str, Any]:
    text = _normalise_text(text)
    entries: list[dict[str, Any]] = []
    for match in ROUTE_RE.finditer(text):
        groups = match.groupdict()
        disposition = groups.get("disposition", "").lower()
        if disposition == "compatibility":
            disposition = "legacy"
        digest = groups.get("digest")
        entries.append({"disposition": disposition, "reason": groups.get("reason"),
                        "mir_digest": digest.lower() if digest else None})
    for match in ROUTE_JSON_RE.finditer(text):
        disposition = match.group("disposition").lower()
        entries.append({"disposition": "legacy" if disposition == "compatibility" else disposition,
                        "reason": None, "mir_digest": None})
    for key, value in _json_key_values(text):
        if key.lower() not in {"route", "route_disposition", "route_kind", "profile"}:
            continue
        candidate = value.get("disposition") if isinstance(value, dict) else value
        if not isinstance(candidate, str) or candidate.lower() not in {"canonical", "legacy", "compatibility", "rejected"}:
            continue
        lowered = candidate.lower()
        digest = value.get("mir_digest") if isinstance(value, dict) else None
        entries.append({"disposition": "legacy" if lowered == "compatibility" else lowered,
                        "reason": value.get("reason") if isinstance(value, dict) else None,
                        "mir_digest": digest.lower() if isinstance(digest, str) and re.fullmatch(r"[0-9a-fA-F]{64}", digest) else None})
    if not entries and ("default Canonical MIR route rejected" in text or "canonical route rejected" in text.lower()):
        entries.append({"disposition": "rejected", "reason": "route-rejected", "mir_digest": None})
    dispositions = {entry["disposition"] for entry in entries}
    digests = {entry["mir_digest"] for entry in entries if entry["mir_digest"] is not None}
    ambiguous = len(dispositions) > 1 or len(digests) > 1
    unique = entries[0] if entries and not ambiguous else {}
    return {
        "observed": bool(entries) and not ambiguous,
        "ambiguous": ambiguous,
        "dispositions": sorted(dispositions),
        "disposition": unique.get("disposition"),
        "reason": unique.get("reason"),
        "mir_digest": unique.get("mir_digest"),
    }


def parse_backend(text: str | bytes) -> dict[str, Any]:
    text = _normalise_text(text)
    aliases = {"ref": "reference", "vm": "bytecode", "llvm": "native", "z3": "verifier", "host_c": "host-c"}
    allowed = {"reference", "bytecode", "native", "verifier", "host_c"}
    entries: list[str] = []
    for match in BACKEND_RE.finditer(text):
        backend = match.group("backend").strip().lower().replace("-", "_").replace(" ", "_")
        backend = aliases.get(backend, backend)
        if backend in allowed:
            entries.append(backend)
    for match in BACKEND_JSON_RE.finditer(text):
        backend = match.group("backend").strip().lower().replace("-", "_").replace(" ", "_")
        backend = aliases.get(backend, backend)
        if backend in allowed:
            entries.append(backend)
    for key, value in _json_key_values(text):
        if key.lower() not in {"backend", "consumer", "executor", "execution_backend"}:
            continue
        candidate = value.get("observed") if isinstance(value, dict) else value
        if not isinstance(candidate, str):
            continue
        backend = candidate.strip().lower().replace("-", "_").replace(" ", "_")
        backend = aliases.get(backend, backend)
        if backend in allowed:
            entries.append(backend)
    unique = set(entries)
    return {"observed": bool(entries) and len(unique) == 1,
            "ambiguous": len(unique) > 1,
            "backends": sorted(unique),
            "backend": next(iter(unique)) if len(unique) == 1 else None}


def parse_validator(text: str | bytes) -> dict[str, Any]:
    text = _normalise_text(text)
    aliases = {"proven": "Proven", "disproven": "Disproven", "passed": "Proven",
               "failed": "InfrastructureError"}
    statuses: list[str] = []
    for match in VALIDATOR_RE.finditer(text):
        value = match.group("status")
        statuses.append(aliases.get(value.lower(), value))
    for match in VALIDATOR_JSON_RE.finditer(text):
        value = match.group("status")
        statuses.append(aliases.get(value.lower(), value))
    for key, value in _json_key_values(text):
        if key.lower() not in {"verifier", "validator", "verifier_status", "validator_status", "proof_status", "verification_status"}:
            continue
        candidate = value.get("observed") if isinstance(value, dict) else value
        if isinstance(candidate, str):
            statuses.append(aliases.get(candidate.lower(), candidate))
    unique = set(statuses)
    return {"observed": bool(statuses) and len(unique) == 1,
            "ambiguous": len(unique) > 1,
            "statuses": sorted(unique),
            "status": next(iter(unique)) if len(unique) == 1 else None}


def parse_abi(text: str | bytes) -> dict[str, Any]:
    text = _normalise_text(text)
    digests = {match.group("digest").lower() for match in ABI_RE.finditer(text)}
    incompatibility = bool(re.search(r"abi\s+(?:check\s+)?(?:failed|mismatch|incompatible)", text, re.IGNORECASE))
    compatibility = bool(re.search(r"abi\s+(?:check\s+)?(?:passed|ok|compatible)", text, re.IGNORECASE))
    observed = bool(digests or incompatibility or compatibility)
    ambiguous = len(digests) > 1 or (incompatibility and compatibility)
    return {
        "observed": observed and not ambiguous,
        "ambiguous": ambiguous,
        "digests": sorted(digests),
        "digest": next(iter(digests)) if len(digests) == 1 else None,
        "compatible": False if incompatibility else (True if compatibility else None),
    }


def parse_native_memory(text: str | bytes) -> dict[str, Any]:
    text = _normalise_text(text)
    rss_values = [int(match.group("rss")) for match in MEMORY_RSS_RE.finditer(text)]
    allocation_values = [int(match.group("count")) for match in MEMORY_ALLOC_RE.finditer(text)]
    valgrind = {"errors": None, "definitely_lost_bytes": None, "indirectly_lost_bytes": None}
    observed_errors: list[int] = []
    observed_definitely_lost: list[int] = []
    observed_indirectly_lost: list[int] = []
    for match in VALGRIND_ERROR_RE.finditer(text):
        groups = match.groupdict()
        if groups.get("errors") is not None:
            observed_errors.append(int(groups["errors"]))
        if groups.get("lost") is not None:
            observed_definitely_lost.append(int(groups["lost"].replace(",", "")))
        if groups.get("indirect") is not None:
            observed_indirectly_lost.append(int(groups["indirect"].replace(",", "")))
    if observed_errors:
        valgrind["errors"] = max(observed_errors)
    if observed_definitely_lost:
        valgrind["definitely_lost_bytes"] = max(observed_definitely_lost)
    if observed_indirectly_lost:
        valgrind["indirectly_lost_bytes"] = max(observed_indirectly_lost)
    sanitizer_errors = SANITIZER_ERROR_RE.findall(text)
    observed = bool(rss_values or allocation_values or any(value is not None for value in valgrind.values()) or sanitizer_errors)
    return {
        "observed": observed,
        "max_rss_kb": max(rss_values) if rss_values else None,
        "allocations": max(allocation_values) if allocation_values else None,
        "valgrind": valgrind,
        "observed_valgrind_errors": observed_errors,
        "sanitizer_errors": sanitizer_errors,
        "memory_errors": bool(
            sanitizer_errors
            or any(value != 0 for value in observed_errors)
            or (valgrind["definitely_lost_bytes"] is not None and valgrind["definitely_lost_bytes"] != 0)
            or (valgrind["indirectly_lost_bytes"] is not None and valgrind["indirectly_lost_bytes"] != 0)
        ),
    }


def parse_observations(text: str | bytes) -> dict[str, Any]:
    text = _normalise_text(text)
    identity = SOURCE_IDENTITY_RE.search(text)
    commit = COMMIT_RE.search(text)
    cargo_finished = bool(CARGO_FINISHED_RE.search(text))
    if not cargo_finished:
        # Cargo does not promise key order in its JSON messages.  Parse each
        # line as a fallback so a valid ``build-finished`` message remains
        # usable when ``success`` precedes ``reason``.
        for line in text.splitlines():
            try:
                message = json.loads(line)
            except (TypeError, json.JSONDecodeError):
                continue
            if isinstance(message, dict) and message.get("reason") == "build-finished" and message.get("success") is True:
                cargo_finished = True
                break
    return {
        "route": parse_route(text),
        "backend": parse_backend(text),
        "validator": parse_validator(text),
        "abi": parse_abi(text),
        "native_memory": parse_native_memory(text),
        "offline": {"network_markers": NETWORK_RE.findall(text),
                     "cargo_build_finished": cargo_finished},
        "provenance": {"observed": bool(identity or commit),
                        "source_identity": identity.group("identity").lower() if identity else None,
                        "commit": commit.group("commit").lower() if commit else None},
    }


def _status_for_observations(
    *,
    exit_code: int | None,
    observations: Mapping[str, Any],
    required: Iterable[str],
    source_stable: bool,
    toolchain_stable: bool,
    offline: bool = False,
    artifacts_ok: bool = True,
    abi_ok: bool = True,
    expected_route: str | None = None,
    expected_backend: str | None = None,
    expected_validator: str | None = None,
    offline_rebuild: bool = False,
    missing_tools: Iterable[str] = (),
) -> tuple[str, str]:
    required = list(dict.fromkeys(required))
    if exit_code is None:
        return "environment_unverified", "command could not be started"
    missing_tools = list(dict.fromkeys(missing_tools))
    if missing_tools:
        return "environment_unverified", "required toolchain components missing: " + ", ".join(missing_tools)
    if not source_stable or not toolchain_stable:
        return "unexplained_failure", "source or toolchain identity changed during evidence"
    if exit_code != 0:
        return "unexplained_failure", f"command exited with status {exit_code}"
    missing = [kind for kind in required if not observations.get(kind, {}).get("observed")]
    if missing:
        return "environment_unverified", "required observations missing: " + ", ".join(missing)
    if offline and observations.get("offline", {}).get("network_markers"):
        return "environment_unverified", "offline command emitted network access markers"
    if not required and not offline_rebuild:
        return "environment_unverified", "no release observations were required"
    if offline_rebuild and not observations.get("offline", {}).get("cargo_build_finished"):
        return "environment_unverified", "offline rebuild has no successful Cargo build-finished marker"
    if expected_route and observations.get("route", {}).get("disposition") != expected_route:
        return "unexplained_failure", "route disposition did not match expectation"
    if "route" in required:
        route = observations.get("route", {})
        if route.get("ambiguous"):
            return "unexplained_failure", "route receipt contains conflicting dispositions"
        if route.get("disposition") != "canonical":
            return "unexplained_failure", "required release route is not Canonical"
        if not isinstance(route.get("mir_digest"), str) or not re.fullmatch(r"[0-9a-fA-F]{64}", route["mir_digest"]):
            return "environment_unverified", "required route receipt has no valid MIR digest"
    if expected_backend and observations.get("backend", {}).get("backend") != expected_backend:
        return "unexplained_failure", "backend did not match expectation"
    if observations.get("backend", {}).get("ambiguous"):
        return "unexplained_failure", "backend receipt contains conflicting consumers"
    if expected_validator and observations.get("validator", {}).get("status") != expected_validator:
        return "unexplained_failure", "verifier status did not match expectation"
    if observations.get("validator", {}).get("ambiguous"):
        return "unexplained_failure", "verifier receipt contains conflicting statuses"
    if not artifacts_ok:
        return "environment_unverified", "required artifact missing or hash mismatch"
    if not abi_ok:
        return "unexplained_failure", "ABI check failed or did not match expected digest"
    memory = observations.get("native_memory", {})
    if memory.get("memory_errors"):
        return "unexplained_failure", "native memory detector reported an error"
    validator = observations.get("validator", {})
    if "validator" in required and validator.get("status") != "Proven":
        status = validator.get("status") or "missing"
        return "unexplained_failure" if status == "Disproven" else "environment_unverified", f"verifier status is {status}"
    if "abi" in required and observations.get("abi", {}).get("compatible") is False:
        return "unexplained_failure", "ABI check reported incompatibility"
    if "abi" in required and observations.get("abi", {}).get("ambiguous"):
        return "unexplained_failure", "ABI receipt contains conflicting results"
    return "passed", "all required observations and identity checks present"


def _artifact_record(path: Path, *, root: Path | None = None, expected_sha256: str | None = None) -> dict[str, Any]:
    record: dict[str, Any] = {"path": _relative_file(root, path) if root else str(path.resolve())}
    if not path.is_file():
        record.update({"observed": False, "status": "missing", "sha256": None})
        return record
    digest = sha256_file(path)
    record.update({"observed": True, "status": "matched" if expected_sha256 in (None, digest) else "mismatch",
                   "sha256": digest, "bytes": path.stat().st_size})
    if expected_sha256 is not None:
        record["expected_sha256"] = expected_sha256
    return record


def _verify_expected_artifacts(root: Path, expected: Mapping[str, str] | None) -> tuple[bool, list[dict[str, Any]]]:
    records: list[dict[str, Any]] = []
    if not expected:
        return True, records
    ok = True
    for name, digest in sorted(expected.items()):
        record = _artifact_record((root / name) if not Path(name).is_absolute() else Path(name), root=root,
                                  expected_sha256=digest)
        records.append(record)
        ok = ok and record["status"] == "matched"
    return ok, records


def _missing_toolchain_components(baseline: Mapping[str, Any], required: Iterable[str],
                                  expected_backend: str | None) -> list[str]:
    """Return explicitly missing toolchain probes relevant to this evidence."""

    rows = baseline.get("toolchain", {}).get("tools", [])
    required_tools: set[str] = set()
    if "validator" in required:
        required_tools.add("z3")
    if "native_memory" in required or (expected_backend or "").lower() in {"native", "llvm"}:
        required_tools.add("llvm")
    if not rows:
        return sorted(required_tools)
    observed = {row.get("name") for row in rows if row.get("status") == "observed"}
    return sorted(required_tools - observed)


def _unique_log_path(evidence: Path, evidence_id: str, suffix: str = ".log") -> Path:
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", evidence_id) or "evidence"
    path = evidence / f"{safe}{suffix}"
    if not path.exists():
        return path
    index = 2
    while True:
        candidate = evidence / f"{safe}.attempt-{index}{suffix}"
        if not candidate.exists():
            return candidate
        index += 1


def _command_executable_record(command: Sequence[str], root: Path, env: Mapping[str, str]) -> dict[str, Any]:
    if not command:
        return {"path": None, "status": "missing", "sha256": None}
    value = command[0]
    candidate = Path(value)
    if candidate.is_absolute() or "/" in value or "\\" in value:
        resolved = (candidate if candidate.is_absolute() else root / candidate).resolve()
    else:
        found = shutil.which(value, path=env.get("PATH"))
        resolved = Path(found).resolve() if found else None
    if resolved is None or not resolved.is_file():
        return {"path": str(resolved) if resolved else value, "status": "missing", "sha256": None}
    return {"path": str(resolved), "status": "observed", "sha256": sha256_file(resolved),
            "bytes": resolved.stat().st_size}


def run_evidence(
    *,
    root: Path | str,
    evidence: Path | str,
    baseline: Mapping[str, Any],
    evidence_id: str,
    command: Sequence[str],
    kind: str = "command",
    required_observations: Iterable[str] = (),
    expected_artifacts: Mapping[str, str] | None = None,
    offline: bool = False,
    expected_abi_digest: str | None = None,
    expected_route: str | None = None,
    expected_backend: str | None = None,
    expected_validator: str | None = None,
    fixture_id: str | None = None,
    route_mode: str | None = None,
    comparison_hash: str | None = None,
    offline_rebuild: bool = False,
    expected_exit: int = 0,
    expected_result: str = "passed",
    timeout: int = 600,
    extra_environment: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Run one evidence command and retain every failure.

    ``required_observations`` may contain ``route``, ``backend``, ``validator``,
    ``native_memory`` and ``abi``.  A successful process with missing markers is
    deliberately ``environment_unverified``.
    """

    if expected_result not in {"passed", "explicit_reject"}:
        raise ValueError(f"unknown expected result: {expected_result}")
    if comparison_hash is not None and not re.fullmatch(r"[0-9a-fA-F]{64}", comparison_hash):
        raise ValueError("comparison_hash must be a 64-character SHA-256")
    root = Path(root).resolve()
    evidence = Path(evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    before = source_snapshot(root, baseline.get("explicit_inputs", ()))
    source_stable = before.get("source_identity") == baseline.get("source_identity")
    baseline_git_status = baseline.get("git_status", "observed" if baseline.get("commit") else "unavailable")
    source_stable = source_stable and baseline_git_status == "observed" and before.get("git_status") == "observed"
    toolchain_stable = before.get("toolchain", {}).get("identity") == baseline.get("toolchain", {}).get("identity")
    started = time.time()
    log_path = _unique_log_path(evidence, evidence_id)
    env = dict(os.environ)
    if offline:
        env["CARGO_NET_OFFLINE"] = "true"
        env["MIMI_OFFLINE"] = "1"
    if extra_environment:
        env.update({key: str(value) for key, value in extra_environment.items()})
    executable = _command_executable_record(command, root, env)
    exit_code: int | None = None
    launch_error: str | None = None
    try:
        with log_path.open("xb") as log:
            try:
                process = subprocess.run(
                    list(command), cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT,
                    timeout=timeout, check=False,
                )
                exit_code = process.returncode
            except subprocess.TimeoutExpired as error:
                launch_error = f"timeout after {timeout}s"
                if error.output:
                    log.write(_normalise_text(error.output).encode())
            except OSError as error:
                launch_error = str(error)
                log.write((launch_error + "\n").encode())
    except OSError as error:
        _write_failure(evidence, evidence_id, f"cannot create evidence log: {error}", command=list(command))
        raise
    ended = time.time()
    after = source_snapshot(root, baseline.get("explicit_inputs", ()))
    source_stable = source_stable and not source_identity_changed(before, after)
    toolchain_stable = toolchain_stable and before.get("toolchain", {}).get("identity") == after.get("toolchain", {}).get("identity")
    text = log_path.read_text(errors="replace")
    observations = parse_observations(text)
    artifacts_ok, artifact_records = _verify_expected_artifacts(root, expected_artifacts)
    abi_ok = True
    if expected_abi_digest is not None:
        abi_ok = observations["abi"].get("digest") == expected_abi_digest.lower()
    required = list(dict.fromkeys(required_observations))
    missing_tools = _missing_toolchain_components(baseline, required, expected_backend)
    status, reason = _status_for_observations(
        exit_code=None if launch_error else exit_code,
        observations=observations,
        required=required,
        source_stable=source_stable,
        toolchain_stable=toolchain_stable,
        offline=offline,
        artifacts_ok=artifacts_ok,
        abi_ok=abi_ok,
        expected_route=expected_route,
        expected_backend=expected_backend,
        expected_validator=expected_validator,
        offline_rebuild=offline_rebuild,
        missing_tools=missing_tools,
    )
    if "native_memory" in required and not expected_artifacts:
        status, reason = "environment_unverified", "native memory observations require at least one hashed final artifact"
    if offline_rebuild:
        executable_name = Path(str(executable.get("path", ""))).name.lower()
        if (executable_name not in {"cargo", "cargo.exe"}
                or len(command) < 2 or command[1] != "build" or "--offline" not in command[2:]):
            status, reason = "environment_unverified", "offline rebuild must invoke the resolved Cargo build command with --offline"
        cargo_probe = next((item for item in baseline.get("toolchain", {}).get("tools", []) if item.get("name") == "cargo"), None)
        if command[1:2] == ["build"] and "--offline" in command[2:]:
            captured_cargo = cargo_probe.get("executable") if isinstance(cargo_probe, dict) else None
            if (not cargo_probe or cargo_probe.get("status") != "observed"
                    or not isinstance(captured_cargo, str)
                    or cargo_probe.get("executable_sha256") != executable.get("sha256")
                    or Path(captured_cargo).resolve() != Path(str(executable.get("path", ""))).resolve()):
                status, reason = "environment_unverified", "offline rebuild executable does not match the captured Cargo tool"
    if expected_result == "explicit_reject" and exit_code == expected_exit:
        route_rejected = observations.get("route", {}).get("disposition") == "rejected"
        verifier_rejected = observations.get("validator", {}).get("status") == "Disproven"
        if route_rejected or verifier_rejected:
            status, reason = "explicit_reject", "expected rejection observed"
    elif expected_result == "passed" and exit_code is not None and exit_code != expected_exit:
        status, reason = "unexplained_failure", f"command exit status {exit_code} differs from expected {expected_exit}"
    if launch_error:
        status = "environment_unverified"
        reason = launch_error
    record: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "evidence_id": evidence_id,
        "kind": kind,
        "fixture_id": fixture_id,
        "route_mode": route_mode,
        "comparison_hash": comparison_hash.lower() if comparison_hash else None,
        "command": list(command),
        "command_executable": executable,
        "cwd": str(root),
        "source_identity": baseline.get("source_identity"),
        "commit": baseline.get("commit"),
        "toolchain_identity": baseline.get("toolchain", {}).get("identity"),
        "source_stable": source_stable,
        "toolchain_stable": toolchain_stable,
        "exit_code": exit_code,
        "launch_error": launch_error,
        "started_at_unix": started,
        "ended_at_unix": ended,
        "elapsed_seconds": ended - started,
        "environment": {key: env[key] for key in sorted(ENV_KEYS) if key in env},
        "offline": offline,
        "observations": observations,
        "required_observations": required,
        "artifacts": artifact_records,
        "artifacts_ok": artifacts_ok,
        "expected_abi_digest": expected_abi_digest,
        "expected_route": expected_route,
        "expected_backend": expected_backend,
        "expected_validator": expected_validator,
        "offline_rebuild": offline_rebuild,
        "expected_exit": expected_exit,
        "expected_result": expected_result,
        "raw_log": {"path": str(log_path), "sha256": sha256_file(log_path), "bytes": log_path.stat().st_size},
        "status": status,
        "result_class": STATUS_LABELS[status],
        "provisional_pass": status in {PASS_STATUS, "explicit_reject"},
        "provisional": True,
        "accepted": False,
        "acceptance_reason": "no verified external runner attestation is available",
        "reason": reason,
        "limitations": ["Log markers are observations; they are not semantic proofs.",
                        "A missing detector/tool is retained as environment_unverified."],
    }
    record["raw_log_path"] = record["raw_log"]["path"]
    record["raw_log_sha256"] = record["raw_log"]["sha256"]
    record_path = evidence / f"{re.sub(r'[^A-Za-z0-9_.-]+', '_', evidence_id)}.json"
    try:
        _immutable_write(record_path, record)
    except FileExistsError:
        failure_path = _write_failure(evidence, evidence_id, "evidence record already exists; retained new attempt",
                                       candidate=record, raw_log=record["raw_log"])
        record["record_path"] = str(failure_path)
        return record
    record["record_path"] = str(record_path)
    if status != PASS_STATUS:
        failure_path = _write_failure(evidence, evidence_id, reason, record_path=str(record_path), raw_log=record["raw_log"],
                                      source_identity=record["source_identity"], status=status)
        record["failure_record"] = str(failure_path)
    return record


def offline_rebuild(
    *,
    root: Path | str,
    evidence: Path | str,
    baseline: Mapping[str, Any],
    evidence_id: str,
    command: Sequence[str],
    expected_artifacts: Mapping[str, str] | None,
    required_observations: Iterable[str] = (),
    timeout: int = 600,
) -> dict[str, Any]:
    """Run and verify an offline rebuild.

    An offline rebuild is incomplete without expected artifact hashes.  This
    explicit wrapper prevents callers from accidentally treating a successful
    command or a stale path in ``target/`` as a reproducible rebuild.
    """

    evidence = Path(evidence).resolve()
    if not expected_artifacts:
        failure = _write_failure(evidence, evidence_id, "offline rebuild requires expected artifact hashes",
                                 source_identity=baseline.get("source_identity"), command=list(command))
        return {
            "schema_version": SCHEMA_VERSION,
            "evidence_id": evidence_id,
            "kind": "offline_rebuild",
            "status": "environment_unverified",
            "result_class": STATUS_LABELS["environment_unverified"],
            "provisional_pass": False,
            "provisional": True,
            "accepted": False,
            "reason": "offline rebuild requires expected artifact hashes",
            "failure_record": str(failure),
        }
    required = list(dict.fromkeys(required_observations))
    return run_evidence(
        root=root,
        evidence=evidence,
        baseline=baseline,
        evidence_id=evidence_id,
        command=command,
        kind="offline_rebuild",
        required_observations=required,
        expected_artifacts=expected_artifacts,
        offline=True,
        offline_rebuild=True,
        timeout=timeout,
    )


def ingest_log(
    *,
    evidence: Path | str,
    baseline: Mapping[str, Any],
    evidence_id: str,
    log_path: Path | str,
    kind: str = "log",
    required_observations: Iterable[str] = (),
    expected_abi_digest: str | None = None,
    source_identity: str | None = None,
    expected_route: str | None = None,
    expected_backend: str | None = None,
    expected_validator: str | None = None,
    exit_code: int | None = None,
    fixture_id: str | None = None,
    route_mode: str | None = None,
) -> dict[str, Any]:
    """Record an existing real log; the log path alone cannot make it green."""

    evidence = Path(evidence).resolve()
    log_path = Path(log_path).resolve()
    if not log_path.is_file():
        failure = _write_failure(evidence, evidence_id, "raw log is missing", raw_log_path=str(log_path),
                                 source_identity=baseline.get("source_identity"))
        return {"evidence_id": evidence_id, "status": "environment_unverified", "accepted": False,
                "result_class": STATUS_LABELS["environment_unverified"], "failure_record": str(failure)}
    text = log_path.read_text(errors="replace")
    observations = parse_observations(text)
    abi_ok = expected_abi_digest is None or observations["abi"].get("digest") == expected_abi_digest.lower()
    expected_source_identity = (baseline.get("source_identity") or "").lower()
    observed_source_identity = observations.get("provenance", {}).get("source_identity")
    provenance_marker_match = bool(observed_source_identity and observed_source_identity == expected_source_identity)
    # A text marker or caller-supplied source hash is not an attestation.  The
    # imported result remains unverified even when the marker happens to match.
    provenance_bound = False
    if exit_code is None:
        observed_status, observed_reason = "environment_unverified", "ingested log has no declared process exit code"
    elif exit_code != 0:
        observed_status, observed_reason = "unexplained_failure", f"declared process exit code was {exit_code}"
    else:
        required = list(required_observations)
        missing_tools = _missing_toolchain_components(baseline, required, expected_backend)
        observed_status, observed_reason = _status_for_observations(
            exit_code=exit_code,
            observations=observations,
            required=required,
            source_stable=True,
            toolchain_stable=True,
            artifacts_ok=True,
            abi_ok=abi_ok,
            expected_route=expected_route,
            expected_backend=expected_backend,
            expected_validator=expected_validator,
            missing_tools=missing_tools,
        )
    status, reason = "environment_unverified", "detached logs are provisional; no trusted runner attestation is available"
    record = {
        "schema_version": SCHEMA_VERSION,
        "evidence_id": evidence_id,
        "kind": kind,
        "fixture_id": fixture_id,
        "route_mode": route_mode,
        "source_identity": baseline.get("source_identity"),
        "commit": baseline.get("commit"),
        "toolchain_identity": baseline.get("toolchain", {}).get("identity"),
        "observations": observations,
        "required_observations": list(required_observations),
        "expected_abi_digest": expected_abi_digest,
        "expected_route": expected_route,
        "expected_backend": expected_backend,
        "expected_validator": expected_validator,
        "exit_code": exit_code,
        "observed_status": observed_status,
        "observed_reason": observed_reason,
        "source_identity_claim": source_identity,
        "provenance_marker_match": provenance_marker_match,
        "provenance_bound": provenance_bound,
        "raw_log": {"path": str(log_path), "sha256": sha256_file(log_path), "bytes": log_path.stat().st_size},
        "status": status,
        "result_class": STATUS_LABELS[status],
        "provisional_pass": False,
        "provisional": True,
        "accepted": False,
        "acceptance_reason": reason,
        "reason": reason,
    }
    record["raw_log_path"] = record["raw_log"]["path"]
    record["raw_log_sha256"] = record["raw_log"]["sha256"]
    record_path = evidence / f"{re.sub(r'[^A-Za-z0-9_.-]+', '_', evidence_id)}.json"
    try:
        _immutable_write(record_path, record)
    except FileExistsError:
        failure = _write_failure(evidence, evidence_id, "evidence record already exists", candidate=record)
        record["failure_record"] = str(failure)
        return record
    if status != PASS_STATUS:
        failure = _write_failure(evidence, evidence_id, reason, record_path=str(record_path), raw_log=record["raw_log"],
                                 source_identity=record["source_identity"], status=status)
        record["failure_record"] = str(failure)
    return record


def check_abi(expected: Path | str, actual: Path | str, *, evidence: Path | str | None = None,
              evidence_id: str = "abi", baseline: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Compare ABI manifests by bytes and record missing inputs as unverified."""

    expected_path, actual_path = Path(expected).resolve(), Path(actual).resolve()
    expected_record = _artifact_record(expected_path)
    actual_record = _artifact_record(actual_path)
    both_present = expected_record.get("observed") and actual_record.get("observed")
    matched = bool(both_present and expected_record.get("sha256") == actual_record.get("sha256"))
    status = "passed" if matched else ("environment_unverified" if not both_present else "unexplained_failure")
    record = {
        "schema_version": SCHEMA_VERSION,
        "evidence_id": evidence_id,
        "kind": "abi",
        "source_identity": baseline.get("source_identity") if baseline else None,
        "commit": baseline.get("commit") if baseline else None,
        "toolchain_identity": baseline.get("toolchain", {}).get("identity") if baseline else None,
        "expected": expected_record,
        "actual": actual_record,
        "status": status,
        "result_class": STATUS_LABELS[status],
        "provisional_pass": matched,
        "provisional": True,
        "accepted": False,
        "acceptance_reason": "ABI comparison is provisional without a verified external runner attestation",
        "reason": "ABI manifests match" if matched else "ABI input missing" if not both_present else "ABI manifests differ",
    }
    if evidence is not None:
        path = Path(evidence).resolve() / f"{evidence_id}.json"
        try:
            _immutable_write(path, record)
        except FileExistsError:
            failure = _write_failure(Path(evidence).resolve(), evidence_id, "ABI record already exists", candidate=record)
            record["failure_record"] = str(failure)
        if not matched:
            failure = _write_failure(Path(evidence).resolve(), evidence_id, record["reason"], candidate=record)
            record["failure_record"] = str(failure)
    return record


def hash_manifest(evidence: Path | str, *, baseline: Mapping[str, Any] | None = None,
                  include_failures: bool = True) -> dict[str, Any]:
    """Hash all evidence records and logs without treating missing paths as green."""

    evidence = Path(evidence).resolve()
    files: list[dict[str, Any]] = []
    missing: list[str] = []
    for path in sorted(evidence.rglob("*")):
        if not path.is_file() or path.name == "hash-manifest.json":
            continue
        if not include_failures and "failures" in path.relative_to(evidence).parts:
            continue
        files.append({"path": str(path.relative_to(evidence)), "sha256": sha256_file(path), "bytes": path.stat().st_size})
    if not files:
        missing.append("<evidence files>")
    if baseline:
        for item in baseline.get("source_files", []):
            if item.get("kind") == "missing" or item.get("status") == "missing":
                missing.append(item.get("path", ""))
        if not baseline.get("commit"):
            missing.append("<source commit>")
    else:
        missing.append("<source manifest>")
    provisional_accepted = not missing
    result = {
        "schema_version": SCHEMA_VERSION,
        "commit": baseline.get("commit") if baseline else None,
        "source_identity": baseline.get("source_identity") if baseline else None,
        "toolchain_identity": baseline.get("toolchain", {}).get("identity") if baseline else None,
        "source_file_hashes": (baseline.get("source_files") if baseline else None) or (baseline.get("sources") if baseline else None),
        "files": files,
        "missing_inputs": sorted(filter(None, missing)),
        "accepted": False,
        "provisional": True,
        "provisional_accepted": provisional_accepted,
        "status": "passed" if provisional_accepted else "environment_unverified",
        "result_class": STATUS_LABELS["passed" if provisional_accepted else "environment_unverified"],
    }
    result["source_manifest_sha256"] = canonical_hash(result["source_file_hashes"]) if result["source_file_hashes"] is not None else None
    result["manifest_sha256"] = canonical_hash({key: value for key, value in result.items() if key != "manifest_sha256"})
    return result


def aggregate_gate(
    evidence: Path | str,
    *,
    required_ids: Iterable[str] = (),
    required_matrix: Iterable[Mapping[str, Any]] = (),
    baseline: Mapping[str, Any] | None = None,
    output: Path | str | None = None,
) -> dict[str, Any]:
    """Aggregate provisional records; unsigned local data never releases."""

    evidence = Path(evidence).resolve()
    required = list(dict.fromkeys(required_ids))
    matrix = [dict(item) for item in required_matrix]
    configuration_errors: list[str] = []
    if not required:
        configuration_errors.append("release gate requires at least one required evidence id")
    if not matrix:
        configuration_errors.append("release gate requires a fixed fixture/backend/route matrix")

    matrix_ids: list[str] = []
    cell_ids: set[str] = set()
    cells: set[tuple[str, str, str]] = set()
    groups: dict[tuple[str, str], set[str]] = {}
    for item in matrix:
        evidence_id = item.get("evidence_id")
        fixture_id = item.get("fixture_id")
        route_mode = item.get("route_mode")
        backend = item.get("backend")
        route = item.get("route")
        comparison_hash = item.get("comparison_hash")
        if not all(isinstance(value, str) and value for value in (evidence_id, fixture_id, route_mode, backend, route)):
            configuration_errors.append("matrix cells need evidence_id, fixture_id, route_mode, backend, and route")
            continue
        if not isinstance(comparison_hash, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", comparison_hash):
            configuration_errors.append(f"matrix cell {evidence_id} has no valid comparison_hash")
        if route != "canonical":
            configuration_errors.append(f"matrix cell {evidence_id} must require canonical route")
        backend = backend.lower().replace("-", "_")
        if backend not in {"bytecode", "native"}:
            configuration_errors.append(f"matrix cell {evidence_id} backend must be bytecode or native")
        cell = (fixture_id, route_mode, backend)
        if evidence_id in cell_ids or cell in cells:
            configuration_errors.append(f"duplicate matrix evidence id or cell: {evidence_id}")
        cell_ids.add(evidence_id)
        cells.add(cell)
        matrix_ids.append(evidence_id)
        groups.setdefault((fixture_id, route_mode), set()).add(backend)
    for group, backends in groups.items():
        if not {"bytecode", "native"}.issubset(backends):
            configuration_errors.append(f"matrix group {group[0]}/{group[1]} must include bytecode and native")
    required = list(dict.fromkeys([*required, *matrix_ids]))

    by_id: dict[str, dict[str, Any]] = {}
    duplicate_ids: set[str] = set()
    for path in sorted(evidence.glob("*.json")):
        if path.name in {"hash-manifest.json", "source-manifest.json", "gate.json"}:
            continue
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(record, dict) and isinstance(record.get("evidence_id"), str):
            if record["evidence_id"] in by_id:
                duplicate_ids.add(record["evidence_id"])
            else:
                by_id[record["evidence_id"]] = record
    missing = [entry for entry in required if entry not in by_id]
    rejected = [
        entry for entry in required
        if entry in by_id and (
            by_id[entry].get("status") not in {PASS_STATUS, "explicit_reject"}
            or by_id[entry].get("provisional_pass") is not True
        )
    ]
    integrity_failures: list[str] = configuration_errors[:]
    integrity_failures.extend(f"{entry}: duplicate evidence id" for entry in sorted(duplicate_ids & set(required)))
    for entry in required:
        record = by_id.get(entry)
        if not record:
            continue
        if record.get("schema_version") != SCHEMA_VERSION:
            integrity_failures.append(f"{entry}: unsupported evidence record schema")
        if not _record_hash_valid(record):
            integrity_failures.append(f"{entry}: record hash missing or invalid")
        if record.get("accepted") is not False or record.get("provisional") is not True:
            integrity_failures.append(f"{entry}: local record must remain provisional and unaccepted")
        status = record.get("status")
        status_passes = status in {PASS_STATUS, "explicit_reject"}
        if record.get("provisional_pass") is not status_passes:
            integrity_failures.append(f"{entry}: provisional_pass does not match the recorded status")
        if status not in TERMINAL_STATUSES:
            integrity_failures.append(f"{entry}: unknown evidence status")
        elif record.get("result_class") != STATUS_LABELS[status]:
            integrity_failures.append(f"{entry}: result class does not match the recorded status")
        if status == "passed" and record.get("kind") != "abi":
            if record.get("exit_code") != record.get("expected_exit", 0) or record.get("exit_code") != 0:
                integrity_failures.append(f"{entry}: passed command has an unexpected exit code")
        if status == "explicit_reject":
            observations = record.get("observations", {})
            route_rejected = observations.get("route", {}).get("disposition") == "rejected"
            verifier_rejected = observations.get("validator", {}).get("status") == "Disproven"
            if (record.get("expected_result") != "explicit_reject"
                    or record.get("exit_code") != record.get("expected_exit")
                    or not (route_rejected or verifier_rejected)):
                integrity_failures.append(f"{entry}: explicit rejection is not supported by its declared result and receipt")
        if record.get("kind") == "log" and not record.get("provenance_bound", False):
            integrity_failures.append(f"{entry}: detached log has no authenticated source binding")
        raw_log = record.get("raw_log") or {}
        raw_path = raw_log.get("path")
        if record.get("kind") != "abi":
            if not raw_path:
                integrity_failures.append(f"{entry}: raw log is required")
            else:
                path = Path(raw_path)
                if not path.is_file() or raw_log.get("sha256") != sha256_file(path):
                    integrity_failures.append(f"{entry}: raw log missing or changed")
        executable = record.get("command_executable")
        if record.get("kind") not in {"abi", "log"}:
            if not isinstance(executable, dict) or executable.get("status") != "observed":
                integrity_failures.append(f"{entry}: command executable identity is missing")
            else:
                executable_path = Path(str(executable.get("path", "")))
                if (not executable_path.is_file()
                        or executable.get("sha256") != sha256_file(executable_path)):
                    integrity_failures.append(f"{entry}: command executable missing or changed")
        for artifact in record.get("artifacts", []):
            path = Path(artifact.get("path", ""))
            if not path.is_absolute() and record.get("cwd"):
                path = Path(record["cwd"]) / path
            if artifact.get("status") in {"matched", "observed"} and (not path.is_file() or artifact.get("sha256") != sha256_file(path)):
                integrity_failures.append(f"{entry}: artifact missing or changed")
        for section in ("expected", "actual"):
            artifact = record.get(section)
            if not isinstance(artifact, dict) or not artifact.get("observed"):
                continue
            path = Path(artifact.get("path", ""))
            if not path.is_absolute() and record.get("cwd"):
                path = Path(record["cwd"]) / path
            if not path.is_file() or artifact.get("sha256") != sha256_file(path):
                integrity_failures.append(f"{entry}: ABI {section} missing or changed")
        if record.get("kind") == "abi" and record.get("status") == "passed":
            expected_abi = record.get("expected", {})
            actual_abi = record.get("actual", {})
            if (not expected_abi.get("observed") or not actual_abi.get("observed")
                    or expected_abi.get("sha256") != actual_abi.get("sha256")):
                integrity_failures.append(f"{entry}: passed ABI record does not contain matching observed hashes")
        if not record.get("source_identity") or not record.get("toolchain_identity"):
            integrity_failures.append(f"{entry}: source/toolchain identity missing")
        if baseline and record.get("source_identity") != baseline.get("source_identity"):
            integrity_failures.append(f"{entry}: source identity differs from baseline")
        if baseline and record.get("toolchain_identity") != baseline.get("toolchain", {}).get("identity"):
            integrity_failures.append(f"{entry}: toolchain identity differs from baseline")

    matrix_failures: list[str] = []
    for item in matrix:
        evidence_id = item.get("evidence_id")
        record = by_id.get(evidence_id) if isinstance(evidence_id, str) else None
        if not record:
            continue
        observations = record.get("observations", {})
        route = observations.get("route", {})
        backend = observations.get("backend", {})
        if record.get("fixture_id") != item.get("fixture_id") or record.get("route_mode") != item.get("route_mode"):
            matrix_failures.append(f"{evidence_id}: fixture or route mode does not match matrix")
        if (record.get("expected_route") != "canonical" or route.get("ambiguous")
                or route.get("disposition") != "canonical"
                or not isinstance(route.get("mir_digest"), str)
                or not re.fullmatch(r"[0-9a-fA-F]{64}", route.get("mir_digest", ""))):
            matrix_failures.append(f"{evidence_id}: canonical route receipt missing or ambiguous")
        expected_backend = str(item.get("backend", "")).lower().replace("-", "_")
        if record.get("expected_backend") != expected_backend or backend.get("ambiguous") or backend.get("backend") != expected_backend:
            matrix_failures.append(f"{evidence_id}: backend receipt missing or mismatched")
        if record.get("comparison_hash") != item.get("comparison_hash"):
            matrix_failures.append(f"{evidence_id}: same-fixture comparison hash does not match matrix")
    if matrix_failures:
        integrity_failures.extend(matrix_failures)

    identities = {by_id[entry].get("source_identity") for entry in required if entry in by_id}
    if baseline:
        identities.add(baseline.get("source_identity"))
    identities.discard(None)
    same_source = len(identities) == 1 and bool(baseline and baseline.get("source_identity") in identities)
    toolchain_identities = {by_id[entry].get("toolchain_identity") for entry in required if entry in by_id}
    if baseline:
        toolchain_identities.add(baseline.get("toolchain", {}).get("identity"))
    toolchain_identities.discard(None)
    same_toolchain = len(toolchain_identities) == 1 and bool(baseline and baseline.get("toolchain", {}).get("identity") in toolchain_identities)
    manifest = hash_manifest(evidence, baseline=baseline)
    provisional_accepted = (
        bool(required)
        and bool(matrix)
        and not missing
        and not rejected
        and not integrity_failures
        and same_source
        and same_toolchain
        and manifest["provisional_accepted"]
    )
    # There is no signature verifier or trusted CI attestation in this tool.
    # A locally consistent matrix is useful evidence, but never release approval.
    result = {
        "schema_version": SCHEMA_VERSION,
        "required_ids": required,
        "required_matrix": matrix,
        "missing_ids": missing,
        "rejected_ids": rejected,
        "integrity_failures": integrity_failures,
        "same_source_identity": same_source,
        "source_identities": sorted(identities),
        "same_toolchain_identity": same_toolchain,
        "toolchain_identities": sorted(toolchain_identities),
        "manifest_sha256": manifest["manifest_sha256"],
        "provisional": True,
        "provisional_accepted": provisional_accepted,
        "accepted": False,
        "status": "environment_unverified" if provisional_accepted else "unexplained_failure",
        "result_class": STATUS_LABELS["environment_unverified" if provisional_accepted else "unexplained_failure"],
        "reason": "no verified external runner attestation is available" if provisional_accepted else "required evidence, matrix, or integrity checks are incomplete",
        "limitation": "This gate checks declared observations and hashes; it cannot authenticate a runner or approve a release.",
    }
    if output is not None:
        path = Path(output).resolve()
        try:
            _immutable_write(path, result)
        except FileExistsError:
            _write_failure(path.parent, "aggregate-gate", "gate output already exists", candidate=result)
            raise
    return result


def _parse_kv(items: Sequence[str]) -> dict[str, str]:
    values: dict[str, str] = {}
    for item in items:
        if "=" not in item:
            raise ValueError(f"expected NAME=VALUE, got {item!r}")
        key, value = item.split("=", 1)
        values[key] = value
    return values


def cli(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=SCRIPT.parent.parent)
    parser.add_argument("--evidence", type=Path, required=True)
    sub = parser.add_subparsers(dest="mode", required=True)
    snap = sub.add_parser("snapshot")
    snap.add_argument("--input", action="append", default=[])
    command = sub.add_parser("command")
    command.add_argument("--id", required=True)
    command.add_argument("--kind", default="command")
    command.add_argument("--require", action="append", default=[])
    command.add_argument("--artifact", action="append", default=[], metavar="PATH=SHA256")
    command.add_argument("--abi-digest")
    command.add_argument("--route", choices=("canonical", "legacy", "rejected"))
    command.add_argument("--backend")
    command.add_argument("--validator")
    command.add_argument("--fixture-id")
    command.add_argument("--route-mode")
    command.add_argument("--comparison-hash")
    command.add_argument("--expected-exit", type=int, default=0)
    command.add_argument("--expect-reject", action="store_true")
    command.add_argument("--offline", action="store_true")
    command.add_argument("--timeout", type=int, default=600)
    command.add_argument("command", nargs=argparse.REMAINDER)
    offline = sub.add_parser("offline-rebuild")
    offline.add_argument("--id", required=True)
    offline.add_argument("--require", action="append", default=[])
    offline.add_argument("--artifact", action="append", default=[], metavar="PATH=SHA256")
    offline.add_argument("--timeout", type=int, default=600)
    offline.add_argument("command", nargs=argparse.REMAINDER)
    ingest = sub.add_parser("ingest")
    ingest.add_argument("--id", required=True)
    ingest.add_argument("--kind", default="log")
    ingest.add_argument("--log", type=Path, required=True)
    ingest.add_argument("--require", action="append", default=[])
    ingest.add_argument("--abi-digest")
    ingest.add_argument("--source-identity")
    ingest.add_argument("--exit-code", type=int)
    ingest.add_argument("--fixture-id")
    ingest.add_argument("--route-mode")
    ingest.add_argument("--route", choices=("canonical", "legacy", "rejected"))
    ingest.add_argument("--backend")
    ingest.add_argument("--validator")
    abi = sub.add_parser("abi")
    abi.add_argument("--id", default="abi")
    abi.add_argument("--expected", type=Path, required=True)
    abi.add_argument("--actual", type=Path, required=True)
    gate = sub.add_parser("gate")
    gate.add_argument("--require-id", action="append", default=[])
    gate.add_argument("--matrix", type=Path, required=True, help="complete JSON fixture/backend/route matrix")
    gate.add_argument("--output", type=Path)
    hashes = sub.add_parser("hash")
    hashes.add_argument("--output", type=Path, default=Path("hash-manifest.json"))
    args = parser.parse_args(argv)
    root, evidence = args.root.resolve(), args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    if args.mode == "snapshot":
        baseline = source_snapshot(root, args.input)
        try:
            _immutable_write(evidence / "source-manifest.json", baseline)
        except FileExistsError:
            _write_failure(evidence, "source-manifest", "source manifest already exists; retained prior snapshot",
                           source_identity=baseline.get("source_identity"))
            return 2
        return 0 if baseline.get("git_status") == "observed" else 2
    baseline_path = evidence / "source-manifest.json"
    if not baseline_path.is_file():
        _write_failure(evidence, "source-manifest", "run snapshot first; source-manifest.json is required",
                       root=str(root))
        raise RuntimeError("run snapshot first; source-manifest.json is required")
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    if not isinstance(baseline, dict):
        raise ValueError("source manifest must be a JSON object")
    _validate_source_manifest(baseline)
    if args.mode == "command":
        command = list(args.command)
        if command[:1] == ["--"]:
            command = command[1:]
        if not command:
            parser.error("command requires a command after --")
        result = run_evidence(root=root, evidence=evidence, baseline=baseline, evidence_id=args.id,
                              command=command, kind=args.kind, required_observations=args.require,
                              expected_artifacts=_parse_kv(args.artifact), offline=args.offline,
                              expected_abi_digest=args.abi_digest, expected_route=args.route,
                              expected_backend=args.backend, expected_validator=args.validator,
                              fixture_id=args.fixture_id, route_mode=args.route_mode,
                              comparison_hash=args.comparison_hash,
                              expected_exit=args.expected_exit,
                              expected_result="explicit_reject" if args.expect_reject else "passed",
                              timeout=args.timeout)
        return 0 if result.get("accepted") else 2
    if args.mode == "offline-rebuild":
        command = list(args.command)
        if command[:1] == ["--"]:
            command = command[1:]
        if not command:
            parser.error("offline-rebuild requires a command after --")
        result = offline_rebuild(root=root, evidence=evidence, baseline=baseline, evidence_id=args.id,
                                 command=command, expected_artifacts=_parse_kv(args.artifact),
                                 required_observations=args.require, timeout=args.timeout)
        return 0 if result.get("accepted") else 2
    if args.mode == "ingest":
        result = ingest_log(evidence=evidence, baseline=baseline, evidence_id=args.id, log_path=args.log,
                            kind=args.kind, required_observations=args.require,
                            expected_abi_digest=args.abi_digest, source_identity=args.source_identity,
                            expected_route=args.route, expected_backend=args.backend,
                            expected_validator=args.validator, exit_code=args.exit_code,
                            fixture_id=args.fixture_id, route_mode=args.route_mode)
        return 0 if result.get("accepted") else 2
    if args.mode == "abi":
        result = check_abi(args.expected, args.actual, evidence=evidence, evidence_id=args.id, baseline=baseline)
        return 0 if result.get("accepted") else 2
    if args.mode == "hash":
        result = hash_manifest(evidence, baseline=baseline)
        output = args.output if args.output.is_absolute() else evidence / args.output
        try:
            _immutable_write(output, result)
        except FileExistsError:
            _write_failure(evidence, "hash-manifest", "hash manifest already exists; retained prior manifest",
                           candidate=result)
            return 2
        return 0 if result.get("accepted") else 2
    if args.mode == "gate":
        matrix_path = args.matrix.resolve() if args.matrix.is_absolute() else (root / args.matrix).resolve()
        matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
        if not isinstance(matrix, list) or any(not isinstance(item, dict) for item in matrix):
            raise ValueError("matrix must be a JSON array of cell objects")
        try:
            matrix_relative = matrix_path.relative_to(root).as_posix()
        except ValueError as error:
            raise ValueError("matrix must be inside the source root") from error
        captured = {item.get("path"): item.get("sha256") for item in baseline.get("source_files", [])}
        if captured.get(matrix_relative) != sha256_file(matrix_path):
            raise ValueError("matrix must be captured by the source snapshot with a matching hash")
        result = aggregate_gate(evidence, required_ids=args.require_id, required_matrix=matrix, baseline=baseline)
        result["matrix_manifest"] = {"path": matrix_relative, "sha256": sha256_file(matrix_path)}
        if args.output:
            output = args.output if args.output.is_absolute() else evidence / args.output
            try:
                _immutable_write(output.resolve(), result)
            except FileExistsError:
                _write_failure(evidence, "aggregate-gate", "gate output already exists; retained prior result", candidate=result)
                return 2
        return 0 if result.get("accepted") else 2
    raise AssertionError(args.mode)


if __name__ == "__main__":
    try:
        raise SystemExit(cli())
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"release evidence rejected: {error}", file=sys.stderr)
        raise SystemExit(2)
