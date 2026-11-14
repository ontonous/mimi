#!/usr/bin/env python3
"""Small, dependency-free helpers for immutable 0.2 evidence manifests.

The existing ``collect-evidence.py`` command records a Cargo build and its
hashed executables.  This module is deliberately independent from Cargo so
that fixture/oracle runs, route probes, and host-C artifacts can share the
same provenance and retention format.  It never changes ``plan.toml`` (or
any other source file) and refuses to overwrite an existing manifest.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
from typing import Any, Iterable, Mapping, Sequence
import tomllib


SCHEMA_VERSION = 2
FIXTURE_SCHEMA_VERSION = 1
MANIFEST_KIND = "mimi-v02-evidence"
RESULT_CLASSES = {
    "已通过",
    "明确拒绝",
    "环境未验证",
    "不支持",
    "未解释失败",
    "未运行",
}
RESULT_ALIASES = {
    "pass": "已通过",
    "passed": "已通过",
    "ok": "已通过",
    "accept": "已通过",
    "accepted": "已通过",
    "reject": "明确拒绝",
    "rejected": "明确拒绝",
    "unsupported": "不支持",
    "environment": "环境未验证",
    "unverified": "环境未验证",
    "fail": "未解释失败",
    "failed": "未解释失败",
    "not-run": "未运行",
    "not_run": "未运行",
}
IDENTIFIER = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*\Z")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: os.PathLike[str] | str) -> str:
    """Hash a file without loading the whole artifact into memory."""

    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def digest_json(value: Any) -> str:
    """Return a stable digest for JSON-compatible data."""

    encoded = json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    return sha256_bytes(encoded)


def normalize_result_class(value: Any, *, default: str = "未解释失败") -> str:
    if value is None:
        value = default
    if not isinstance(value, str):
        raise ValueError("result class must be text")
    normalized = RESULT_ALIASES.get(value.strip().lower(), value.strip())
    if normalized not in RESULT_CLASSES:
        raise ValueError(f"unknown result class: {value}")
    return normalized


def _git(root: Path, *args: str) -> tuple[int, bytes, bytes]:
    try:
        process = subprocess.run(
            ["git", *args],
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        return 127, b"", str(error).encode("utf-8", errors="replace")
    return process.returncode, process.stdout, process.stderr


def _git_text(root: Path, *args: str) -> str | None:
    code, stdout, _ = _git(root, *args)
    if code:
        return None
    return stdout.decode("utf-8", errors="replace").strip()


def _relative(path: Path, root: Path) -> str:
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return str(path.resolve())


def source_file_record(path: Path, root: Path | None = None) -> dict[str, Any]:
    """Describe one explicitly selected source/input file.

    Missing files are retained as ``{"missing": true}`` rather than silently
    dropped.  This makes a fixture typo visible in the resulting evidence.
    """

    path = Path(path)
    record: dict[str, Any] = {"path": _relative(path, root) if root else str(path.resolve())}
    if not path.exists() or not path.is_file():
        record["missing"] = True
        return record
    record.update(
        sha256=sha256_file(path),
        bytes=path.stat().st_size,
        mode=path.stat().st_mode & 0o777,
    )
    return record


def source_snapshot(root: Path, paths: Iterable[Path | str]) -> dict[str, Any]:
    """Hash explicit source inputs and the current git boundary.

    ``paths`` is intentionally explicit.  A temporary file at repository root
    therefore cannot become release evidence merely because it exists.
    """

    root = Path(root).resolve()
    selected: dict[str, dict[str, Any]] = {}
    for value in paths:
        path = Path(value)
        if not path.is_absolute():
            path = root / path
        key = _relative(path, root)
        selected[key] = source_file_record(path, root)
    commit = _git_text(root, "rev-parse", "HEAD")
    branch = _git_text(root, "branch", "--show-current")
    code, diff, _ = _git(root, "diff", "HEAD", "--binary")
    if code:
        diff_hash = None
    else:
        diff_hash = sha256_bytes(diff)
    status = _git_text(root, "status", "--porcelain=v1")
    snapshot: dict[str, Any] = {
        "root": str(root),
        "commit": commit,
        "branch": branch,
        "tracked_diff_sha256": diff_hash,
        "working_tree_status": status or "",
        "inputs": {key: selected[key] for key in sorted(selected)},
    }
    snapshot["source_identity"] = digest_json(
        {
            "commit": commit,
            "tracked_diff_sha256": diff_hash,
            "inputs": snapshot["inputs"],
        }
    )
    return snapshot


def artifact_record(
    path: Path | str,
    *,
    root: Path | None = None,
    kind: str | None = None,
    required: bool = True,
) -> dict[str, Any]:
    """Hash one final/native/runtime/FFI artifact.

    The absolute path is retained for local verification while ``path`` is
    repository/evidence relative when possible.  Callers should pass only
    files they deliberately selected as evidence artifacts.
    """

    original = Path(path)
    resolved = original.resolve()
    result: dict[str, Any] = {
        "path": _relative(resolved, root) if root else str(resolved),
        "absolute_path": str(resolved),
        "required": required,
    }
    if kind:
        result["kind"] = kind
    if not resolved.is_file():
        result.update({"status": "missing", "sha256": None, "bytes": None})
        return result
    result.update(
        {
            "status": "observed",
            "sha256": sha256_file(resolved),
            "bytes": resolved.stat().st_size,
            "mode": resolved.stat().st_mode & 0o777,
        }
    )
    return result


def verify_artifact_records(
    artifacts: Iterable[Mapping[str, Any]],
    *,
    root: Path | None = None,
) -> list[str]:
    """Return integrity errors for artifact records; an empty list is green."""

    errors: list[str] = []
    for entry in artifacts:
        absolute = entry.get("absolute_path")
        path = Path(absolute) if isinstance(absolute, str) else None
        if path is None:
            value = entry.get("path")
            if not isinstance(value, str):
                errors.append("artifact has no path")
                continue
            path = Path(value)
            if not path.is_absolute() and root:
                path = root / path
        expected = entry.get("sha256")
        if expected is None:
            if entry.get("required", True):
                errors.append(f"missing required artifact: {entry.get('path', path)}")
            continue
        if not path.is_file():
            errors.append(f"artifact is missing: {path}")
            continue
        actual = sha256_file(path)
        if actual != expected:
            errors.append(f"artifact changed: {path}")
    return errors


def environment_snapshot(
    keys: Iterable[str] = (),
    *,
    environment: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Capture explicitly named and toolchain-relevant environment values."""

    source_environment = os.environ if environment is None else environment
    names = set(keys)
    names.update(
        key
        for key in source_environment
        if key.startswith(("MIMI_", "CARGO_TARGET_", "CARGO_PROFILE_"))
        or key in {
            "LLVM_SYS_181_PREFIX",
            "RUSTFLAGS",
            "CC",
            "CXX",
            "RUSTC",
            "RUSTC_WRAPPER",
            "CARGO_BUILD_JOBS",
            "PATH",
        }
    )
    # Do not copy credentials or arbitrary process environment into release
    # evidence.  Explicit fixture names are still allowed, except secret-like
    # names, which are always redacted.
    result: dict[str, str] = {}
    for key in sorted(names):
        if any(token in key.upper() for token in ("TOKEN", "PASSWORD", "SECRET", "API_KEY", "PRIVATE_KEY")):
            result[key] = "<redacted>"
        elif key in source_environment:
            result[key] = source_environment[key]
    return result


def plan_record(plan_path: Path | str | None) -> dict[str, Any]:
    if plan_path is None:
        return {"status": "not-specified", "path": None, "sha256": None}
    path = Path(plan_path).resolve()
    if not path.is_file():
        return {"status": "missing", "path": str(path), "sha256": None}
    return {"status": "observed", "path": str(path), "sha256": sha256_file(path)}


def plan_task_ids(plan_path: Path | str | None) -> set[str]:
    """Read task IDs for traceability without changing planning state."""

    if plan_path is None:
        return set()
    path = Path(plan_path)
    if not path.is_file():
        return set()
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return set()
    rows = data.get("tasks", [])
    return {row["id"] for row in rows if isinstance(row, dict) and isinstance(row.get("id"), str)}


def create_manifest(
    *,
    root: Path | str,
    evidence_dir: Path | str,
    plan_path: Path | str | None = None,
    source_paths: Iterable[Path | str] = (),
    records: Iterable[Mapping[str, Any]] = (),
    artifacts: Iterable[Mapping[str, Any]] = (),
    task_ids: Iterable[str] = (),
    fixture_manifest: Mapping[str, Any] | None = None,
    expected_fixture_ids: Iterable[str] = (),
    errors: Iterable[str] = (),
    toolchain: Mapping[str, Any] | None = None,
    metadata: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a manifest object; use :func:`write_manifest` to persist it."""

    root = Path(root).resolve()
    evidence_dir = Path(evidence_dir).resolve()
    frozen_records: list[dict[str, Any]] = []
    for item in records:
        frozen = dict(item)
        supplied_hash = frozen.pop("record_hash", None)
        record_hash = digest_json(frozen)
        if supplied_hash is not None and supplied_hash != record_hash:
            raise ValueError(f"record hash does not match: {frozen.get('fixture_id', frozen.get('evidence_id', '<unknown>'))}")
        frozen["record_hash"] = record_hash
        frozen_records.append(frozen)
    frozen_artifacts = [dict(item) for item in artifacts]
    expected_ids = list(expected_fixture_ids)
    frozen_errors = list(errors)
    plan = plan_record(plan_path)
    source = source_snapshot(root, source_paths)
    classes = {label: 0 for label in sorted(RESULT_CLASSES)}
    for record in frozen_records:
        label = normalize_result_class(record.get("result_class"), default="未运行")
        classes[label] += 1
    record_ids = [item.get("fixture_id", item.get("evidence_id")) for item in frozen_records]
    complete = bool(expected_ids) and len(set(expected_ids)) == len(expected_ids) and set(record_ids) == set(expected_ids) and len(record_ids) == len(expected_ids)
    provisional_accepted = complete and not frozen_errors and all(
        bool(item.get("checks_passed", item.get("accepted", False))) for item in frozen_records
    )
    # Local collection has no trusted-runner attestation.  It may report that
    # declared checks matched, but it must never issue release acceptance.
    accepted = False
    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "kind": MANIFEST_KIND,
        "created_at_utc": datetime.now(timezone.utc).isoformat(),
        "root": str(root),
        "evidence_dir": str(evidence_dir),
        "accepted": accepted,
        "provisional": True,
        "provisional_accepted": provisional_accepted,
        "acceptance_reason": "no verified external runner attestation is available",
        "result_class": "已通过" if provisional_accepted else ("未运行" if not frozen_records else "未解释失败"),
        "result_counts": classes,
        "task_ids": sorted(set(task_ids)),
        "plan": plan,
        "plan_hash": plan.get("sha256"),
        "source": source,
        "fixture_manifest": dict(fixture_manifest) if fixture_manifest else None,
        "toolchain": dict(toolchain) if toolchain else {"status": "not-collected"},
        "commit": source.get("commit"),
        "tracked_diff_hash": source.get("tracked_diff_sha256"),
        "source_identity": source.get("source_identity"),
        "records": frozen_records,
        "expected_fixture_ids": expected_ids,
        "errors": frozen_errors,
        "artifacts": frozen_artifacts,
        "environment": environment_snapshot(),
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
        "limitations": [
            "Explicit source inputs are hashed; unrelated untracked files are not evidence.",
            "A passing fixture compares its declared oracle/expectation; this does not prove an unlisted semantic shape.",
            "Artifact hashes do not establish external tool trust or L3 memory safety by themselves.",
        ],
    }
    if metadata:
        result["metadata"] = dict(metadata)
    result["manifest_content_hash"] = digest_json({key: value for key, value in result.items() if key != "manifest_content_hash"})
    return result


def write_manifest(path: Path | str, manifest: Mapping[str, Any], *, write_digest: bool = True) -> Path:
    """Write one immutable JSON manifest and an optional detached hash."""

    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    digest_path = path.with_name(path.name + ".sha256")
    if path.exists() or (write_digest and digest_path.exists()):
        raise FileExistsError(path)
    data = dict(manifest)
    supplied_hash = data.pop("manifest_content_hash", None)
    content_hash = digest_json(data)
    if supplied_hash != content_hash:
        raise ValueError("manifest content hash does not match manifest content")
    data["manifest_content_hash"] = supplied_hash
    payload = json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False).encode("utf-8") + b"\n"
    with path.open("xb") as stream:
        stream.write(payload)
    if write_digest:
        with digest_path.open("xb") as stream:
            stream.write(sha256_bytes(payload).encode("ascii") + b"\n")
    return path


def verify_manifest(
    path: Path | str,
    *,
    root: Path | str | None = None,
    verify_source: bool = False,
) -> dict[str, Any]:
    """Verify an immutable manifest and its referenced artifacts.

    The function raises ``ValueError`` for malformed manifests and returns a
    compact report for callers/CI.  It never rewrites the manifest.
    """

    path = Path(path)
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != SCHEMA_VERSION or data.get("kind") != MANIFEST_KIND:
        raise ValueError("unsupported evidence manifest schema")
    if data.get("accepted") is not False:
        raise ValueError("unsigned local evidence cannot be accepted for release")
    if data.get("provisional") is not True:
        raise ValueError("evidence manifest must be explicitly provisional")
    records = data.get("records")
    artifacts = data.get("artifacts")
    expected_ids = data.get("expected_fixture_ids")
    if not isinstance(records, list) or not records:
        raise ValueError("evidence manifest must contain fixture records")
    if not isinstance(artifacts, list) or not artifacts:
        raise ValueError("evidence manifest must contain artifact records")
    if not isinstance(expected_ids, list) or not expected_ids or any(not isinstance(item, str) for item in expected_ids):
        raise ValueError("evidence manifest must declare its complete fixture inventory")
    record_ids: list[str] = []
    for record in records:
        if not isinstance(record, dict):
            raise ValueError("fixture records must be objects")
        fixture_id = record.get("fixture_id", record.get("evidence_id"))
        if not isinstance(fixture_id, str) or not fixture_id:
            raise ValueError("fixture record has no id")
        record_ids.append(fixture_id)
        expected_record_hash = record.get("record_hash")
        record_body = {key: value for key, value in record.items() if key != "record_hash"}
        if not isinstance(expected_record_hash, str) or expected_record_hash != digest_json(record_body):
            raise ValueError(f"fixture record hash does not match: {fixture_id}")
        if record.get("accepted") is not False:
            raise ValueError(f"fixture record is not marked provisional: {fixture_id}")
        if not isinstance(record.get("checks_passed"), bool):
            raise ValueError(f"fixture record has no checks_passed result: {fixture_id}")
    if len(set(record_ids)) != len(record_ids):
        raise ValueError("duplicate fixture records")
    complete = len(record_ids) == len(expected_ids) and set(record_ids) == set(expected_ids)
    classes = {label: 0 for label in sorted(RESULT_CLASSES)}
    for record in records:
        classes[normalize_result_class(record.get("result_class"), default="未运行")] += 1
    errors = data.get("errors")
    if not isinstance(errors, list) or any(not isinstance(item, str) for item in errors):
        raise ValueError("manifest errors must be a text array")
    provisional_accepted = complete and not errors and all(record["checks_passed"] for record in records)
    if data.get("provisional_accepted") is not provisional_accepted:
        raise ValueError("manifest provisional_accepted does not match fixture records and errors")
    if data.get("result_counts") != classes:
        raise ValueError("manifest result counts do not match fixture records")
    expected_result_class = "已通过" if provisional_accepted else ("未运行" if not records else "未解释失败")
    if data.get("result_class") != expected_result_class:
        raise ValueError("manifest result class does not match fixture records and completeness")
    content_body = {key: value for key, value in data.items() if key != "manifest_content_hash"}
    if data.get("manifest_content_hash") != digest_json(content_body):
        raise ValueError("manifest content hash does not match manifest content")
    digest_path = path.with_name(path.name + ".sha256")
    if not digest_path.is_file():
        raise ValueError("manifest detached digest is missing")
    expected = digest_path.read_text(encoding="ascii").strip()
    digest_ok = expected == sha256_file(path)
    if not digest_ok:
        raise ValueError("manifest digest does not match manifest bytes")
    artifact_errors = verify_artifact_records(artifacts, root=Path(root) if root else None)
    if artifact_errors:
        raise ValueError("; ".join(artifact_errors))
    source_ok: bool | None = None
    if verify_source:
        source = data.get("source", {})
        inputs = [entry.get("path") for entry in source.get("inputs", {}).values() if entry.get("path")]
        current = source_snapshot(Path(root or data.get("root", path.parent)), inputs)
        source_ok = current.get("source_identity") == source.get("source_identity")
        if not source_ok:
            raise ValueError("source identity changed since evidence collection")
    return {
        "manifest": str(path.resolve()),
        "digest_ok": digest_ok,
        "artifact_count": len(artifacts),
        "record_count": len(records),
        "source_ok": source_ok,
        "accepted": False,
        "provisional_accepted": provisional_accepted,
    }


def _validate_command(command: Any, label: str) -> list[str]:
    if not isinstance(command, list) or not command or any(not isinstance(item, str) or not item for item in command):
        raise ValueError(f"{label} must be a non-empty argv list")
    return list(command)


def load_fixture_manifest(path: Path | str) -> dict[str, Any]:
    """Load and validate a fixture manifest consumed by ``run-fixtures.py``."""

    path = Path(path)
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != FIXTURE_SCHEMA_VERSION:
        raise ValueError("fixture manifest schema_version must be 1")
    fixtures = data.get("fixtures")
    if not isinstance(fixtures, list) or not fixtures:
        raise ValueError("fixture manifest needs a non-empty fixtures array")
    seen: set[str] = set()
    for fixture in fixtures:
        if not isinstance(fixture, dict):
            raise ValueError("fixture entries must be objects")
        fixture_id = fixture.get("id")
        if not isinstance(fixture_id, str) or not IDENTIFIER.fullmatch(fixture_id):
            raise ValueError("fixture id must use letters, digits, dots, dashes or underscores")
        if fixture_id in seen:
            raise ValueError(f"duplicate fixture id: {fixture_id}")
        seen.add(fixture_id)
        candidate = fixture.get("command", fixture.get("candidate"))
        if isinstance(candidate, dict):
            candidate = candidate.get("command")
        _validate_command(candidate, f"fixture {fixture_id} command")
        inputs = fixture.get("inputs", [])
        if not isinstance(inputs, list) or any(not isinstance(item, str) or not item for item in inputs):
            raise ValueError(f"fixture {fixture_id} inputs must be paths")
        artifacts = fixture.get("artifacts", [])
        if not isinstance(artifacts, list):
            raise ValueError(f"fixture {fixture_id} artifacts must be an array")
        expected = fixture.get("expected", {})
        if not isinstance(expected, dict):
            raise ValueError(f"fixture {fixture_id} expected must be an object")
        oracle = fixture.get("oracle")
        if oracle is not None and isinstance(oracle, dict) and "command" in oracle:
            _validate_command(oracle["command"], f"fixture {fixture_id} oracle command")
        declared = expected.get("result_class", expected.get("status", "已通过"))
        normalize_result_class(declared, default="已通过")
    return data


__all__ = [
    "MANIFEST_KIND",
    "RESULT_CLASSES",
    "SCHEMA_VERSION",
    "artifact_record",
    "create_manifest",
    "digest_json",
    "environment_snapshot",
    "load_fixture_manifest",
    "normalize_result_class",
    "plan_record",
    "plan_task_ids",
    "sha256_bytes",
    "sha256_file",
    "source_file_record",
    "source_snapshot",
    "verify_artifact_records",
    "verify_manifest",
    "write_manifest",
]
