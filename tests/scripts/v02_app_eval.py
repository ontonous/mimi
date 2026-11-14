#!/usr/bin/env python3
"""Run the V0.2 P6 application fixtures through real Mimi consumers.

The runner intentionally does not emulate Mimi.  Every fixture is passed to
the checked-in ``mimi`` executable for ``check`` and ``run``; native cases are
also compiled and executed.  A small Python oracle computes expected values
from the frozen requirements, and a separately compiled C helper supplies the
W4/S1/S4 FFI symbol.  Native capability gaps are reported as
``unsupported``/``environment_unverified`` rather than silently counted as a
pass.  ``--strict`` turns those observations into a non-zero exit status for
release gating.
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
import tempfile
from pathlib import Path
from pathlib import PurePosixPath
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests" / "fixtures" / "v02_app"
MANIFEST_PATH = FIXTURES / "manifest.json"
EXPECTED_PATH = FIXTURES / "oracle" / "v02_expected.json"


EXPECTED_SEQUENCES = {
    "SEQ-W2": ["W2", "D1", "D2", "D3", "D4", "D5"],
    "SEQ-W4": ["W4", "S1", "S2", "S3", "S4"],
}
EXPECTED_BASELINES = {
    "SEQ-W2": {"root": "w2", "entrypoint": "w2_document_core.mimi"},
    "SEQ-W4": {"root": "w4", "entrypoint": "baseline/src/main.mimi"},
}
EXPECTED_PATCH_PATHS = {
    "SEQ-W2": {fixture: f"w2/sequences/patches/{fixture}.patch" for fixture in EXPECTED_SEQUENCES["SEQ-W2"][1:]},
    "SEQ-W4": {fixture: f"w4/sequences/patches/{fixture}.patch" for fixture in EXPECTED_SEQUENCES["SEQ-W4"][1:]},
}
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?:.*)$")


class SequenceError(ValueError):
    """A sequence cannot be replayed without weakening its evidence contract."""


def sha256_bytes(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def safe_relative_path(value: Any, *, field: str) -> Path:
    if not isinstance(value, str) or not value or "\\" in value:
        raise SequenceError(f"{field} must be a non-empty POSIX relative path")
    if "\x00" in value:
        raise SequenceError(f"{field} must be a non-empty POSIX relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise SequenceError(f"{field} escapes its declared root: {value!r}")
    return Path(*path.parts)


def confined_file(root: Path, relative: Any, *, field: str) -> Path:
    path = root / safe_relative_path(relative, field=field)
    try:
        path.resolve(strict=False).relative_to(root.resolve())
    except ValueError as error:
        raise SequenceError(f"{field} escapes its declared root: {relative!r}") from error
    return path


def tree_digest(root: Path) -> str:
    """Hash a source tree independently of mtimes, permissions, and traversal order."""

    if not root.is_dir():
        raise SequenceError(f"cumulative worktree is missing: {root}")
    digest_state = hashlib.sha256()
    files = sorted(root.rglob("*"), key=lambda path: path.relative_to(root).as_posix())
    for path in files:
        if path.is_symlink():
            raise SequenceError(f"symlinks are not valid sequence inputs: {path.relative_to(root)}")
        if not path.is_file():
            continue
        relative = path.relative_to(root).as_posix().encode("utf-8")
        contents_hash = bytes.fromhex(sha256_bytes(path.read_bytes()))
        digest_state.update(len(relative).to_bytes(8, "big"))
        digest_state.update(relative)
        digest_state.update(contents_hash)
    return digest_state.hexdigest()


def tree_file_hashes(root: Path) -> dict[str, str]:
    """Return the regular-file inventory used to record cumulative mutations."""

    if not root.is_dir():
        raise SequenceError(f"cumulative worktree is missing: {root}")
    result: dict[str, str] = {}
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        if path.is_symlink():
            raise SequenceError(f"symlinks are not valid sequence inputs: {path.relative_to(root)}")
        if path.is_file():
            result[path.relative_to(root).as_posix()] = digest(path)
    return result


def _record_cumulative_mutations(
    result: dict[str, Any], *, before: dict[str, str], after: dict[str, str]
) -> None:
    changed = []
    for path in sorted(set(before) | set(after)):
        old = before.get(path)
        new = after.get(path)
        if old != new:
            changed.append({"path": path, "before_sha256": old, "after_sha256": new})
    result["cumulative_state_changes"] = changed
    result["cumulative_state_before_sha256"] = _hash_inventory(before)
    result["cumulative_state_after_sha256"] = _hash_inventory(after)


def _hash_inventory(inventory: dict[str, str]) -> str:
    state = hashlib.sha256()
    for path, content_hash in sorted(inventory.items()):
        encoded = path.encode("utf-8")
        state.update(len(encoded).to_bytes(8, "big"))
        state.update(encoded)
        state.update(bytes.fromhex(content_hash))
    return state.hexdigest()


def _patch_path(raw: str) -> Path | None:
    path = raw.split("\t", 1)[0].strip()
    if path == "/dev/null":
        return None
    if path.startswith(("a/", "b/")):
        path = path[2:]
    return safe_relative_path(path, field="unified diff path")


def _apply_hunks(source: list[str], hunks: list[tuple[int, int, int, int, list[tuple[str, str, bool]]]], path: Path) -> list[str]:
    result: list[str] = []
    cursor = 0
    for old_start, old_count, new_start, new_count, body in hunks:
        old_index = old_start if old_count == 0 else old_start - 1
        if old_index < cursor or old_index > len(source):
            raise SequenceError(f"patch hunks overlap or address missing lines in {path}")
        result.extend(source[cursor:old_index])
        cursor = old_index
        rendered_count = 0
        consumed = 0
        rendered: list[str] = []
        for action, contents, no_newline in body:
            expected = contents[:-1] if no_newline and contents.endswith("\n") else contents
            if action in (" ", "-"):
                if cursor >= len(source) or source[cursor] != expected:
                    raise SequenceError(f"patch context mismatch in {path} at source line {cursor + 1}")
                cursor += 1
                consumed += 1
            if action in (" ", "+"):
                rendered.append(expected)
                rendered_count += 1
        if consumed != old_count or rendered_count != new_count:
            raise SequenceError(f"patch hunk line counts do not match in {path}")
        expected_new_index = new_start if new_count == 0 else new_start - 1
        if expected_new_index != len(result):
            raise SequenceError(f"patch output position mismatch in {path}")
        result.extend(rendered)
    result.extend(source[cursor:])
    return result


def apply_unified_diff(worktree: Path, patch_contents: bytes) -> list[dict[str, Any]]:
    """Apply a strict, text-only unified diff; context is exact and never fuzzed."""

    try:
        lines = patch_contents.decode("utf-8").splitlines(keepends=True)
    except UnicodeDecodeError as error:
        raise SequenceError("unified diff is not UTF-8 text") from error
    changes: list[dict[str, Any]] = []
    seen: set[Path] = set()
    index = 0
    while index < len(lines):
        if not lines[index].startswith("--- "):
            if lines[index].startswith(("diff --git ", "index ", "new file mode ", "deleted file mode ")):
                index += 1
                continue
            if lines[index].strip() == "":
                index += 1
                continue
            raise SequenceError(f"unsupported unified diff record: {lines[index].rstrip()!r}")
        old_relative = _patch_path(lines[index][4:])
        index += 1
        if index >= len(lines) or not lines[index].startswith("+++ "):
            raise SequenceError("unified diff is missing a +++ file header")
        new_relative = _patch_path(lines[index][4:])
        index += 1
        if old_relative is not None and new_relative is not None and old_relative != new_relative:
            raise SequenceError("renames are not supported in sequence patches")
        relative = new_relative or old_relative
        if relative is None:
            raise SequenceError("a unified diff cannot change /dev/null on both sides")
        if relative in seen:
            raise SequenceError(f"a sequence patch changes {relative} more than once")
        seen.add(relative)
        target = confined_file(worktree, relative.as_posix(), field="unified diff path")
        if target.is_symlink():
            raise SequenceError(f"sequence patch target is a symlink: {relative}")
        if old_relative is None:
            if target.exists():
                raise SequenceError(f"patch add target already exists: {relative}")
            before = b""
            source_lines: list[str] = []
        else:
            if not target.is_file():
                raise SequenceError(f"patch source does not exist as a file: {relative}")
            before = target.read_bytes()
            try:
                source_lines = before.decode("utf-8").splitlines(keepends=True)
            except UnicodeDecodeError as error:
                raise SequenceError(f"patch source is not UTF-8 text: {relative}") from error
        hunks: list[tuple[int, int, int, int, list[tuple[str, str, bool]]]] = []
        while index < len(lines) and not lines[index].startswith("--- "):
            match = HUNK_RE.match(lines[index].rstrip("\n"))
            if not match:
                if lines[index].startswith(("diff --git ", "index ", "new file mode ", "deleted file mode ")):
                    break
                raise SequenceError(f"unsupported unified diff hunk line: {lines[index].rstrip()!r}")
            old_start = int(match.group(1))
            old_count = int(match.group(2) or "1")
            new_start = int(match.group(3))
            new_count = int(match.group(4) or "1")
            index += 1
            body: list[tuple[str, str, bool]] = []
            consumed = 0
            added = 0
            while consumed < old_count or added < new_count:
                if index >= len(lines):
                    raise SequenceError(f"truncated unified diff hunk for {relative}")
                line = lines[index]
                if line.startswith("\\ No newline at end of file"):
                    if not body:
                        raise SequenceError("orphan no-newline marker in unified diff")
                    action, contents, _ = body[-1]
                    body[-1] = (action, contents, True)
                    index += 1
                    continue
                if not line or line[0] not in " +-":
                    raise SequenceError(f"invalid unified diff body for {relative}: {line.rstrip()!r}")
                action = line[0]
                body.append((action, line[1:], False))
                consumed += int(action in " -")
                added += int(action in " +")
                if consumed > old_count or added > new_count:
                    raise SequenceError(f"unified diff hunk line counts overflow for {relative}")
                index += 1
            if index < len(lines) and lines[index].startswith("\\ No newline at end of file"):
                if not body:
                    raise SequenceError("orphan no-newline marker in unified diff")
                action, contents, _ = body[-1]
                body[-1] = (action, contents, True)
                index += 1
            hunks.append((old_start, old_count, new_start, new_count, body))
        if not hunks:
            raise SequenceError(f"unified diff has no hunks for {relative}")
        rendered = _apply_hunks(source_lines, hunks, relative)
        after = "".join(rendered).encode("utf-8")
        if new_relative is None and after:
            raise SequenceError(f"patch deletion leaves content in {relative}")
        changes.append({
            "path": relative.as_posix(),
            "before_sha256": sha256_bytes(before) if old_relative is not None else None,
            "after_sha256": sha256_bytes(after) if new_relative is not None else None,
            "_target": target,
            "_contents": after if new_relative is not None else None,
        })
    if not changes:
        raise SequenceError("unified diff contains no file changes")

    # All context and path checks finish before the worktree is changed.
    for change in changes:
        target = change.pop("_target")
        contents = change.pop("_contents")
        if contents is None:
            target.unlink()
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            temporary = target.with_name(target.name + ".sequence-patch-tmp")
            if temporary.exists():
                raise SequenceError(f"stale patch temporary exists: {temporary}")
            try:
                temporary.write_bytes(contents)
                os.replace(temporary, target)
            finally:
                temporary.unlink(missing_ok=True)
    return changes


def _verify_sha256(value: Any, *, field: str) -> str:
    if not isinstance(value, str) or not SHA256_RE.fullmatch(value):
        raise SequenceError(f"{field} must be a lowercase SHA-256 digest")
    return value


def _manifest_entrypoints(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    entries = [*manifest.get("apps", []), *manifest.get("changes", [])]
    result: dict[str, dict[str, Any]] = {}
    for entry in entries:
        case_id = entry.get("id")
        if not isinstance(case_id, str) or case_id in result:
            raise SequenceError(f"duplicate or invalid fixture id: {case_id!r}")
        result[case_id] = entry
    return result


def _copy_baseline(sequence: dict[str, Any], worktree: Path, fixtures_root: Path) -> list[dict[str, str]]:
    baseline = sequence.get("baseline")
    if not isinstance(baseline, dict):
        raise SequenceError("sequence baseline is missing")
    root = confined_file(fixtures_root, baseline.get("root"), field="baseline root")
    if root.is_symlink() or not root.is_dir():
        raise SequenceError(f"baseline root does not exist: {baseline.get('root')!r}")
    manifest = baseline.get("source_manifest")
    if not isinstance(manifest, list) or not manifest:
        raise SequenceError("baseline source_manifest must enumerate at least one file")
    copied: list[dict[str, str]] = []
    seen: set[str] = set()
    for item in manifest:
        if not isinstance(item, dict):
            raise SequenceError("baseline source_manifest entries must be objects")
        role = item.get("role")
        if not isinstance(role, str) or not role:
            raise SequenceError(f"baseline source_manifest has invalid role for {item.get('path')!r}")
        relative = safe_relative_path(item.get("path"), field="baseline source path")
        relative_text = relative.as_posix()
        if relative_text in seen:
            raise SequenceError(f"baseline source_manifest repeats {relative_text}")
        seen.add(relative_text)
        expected_hash = _verify_sha256(item.get("sha256"), field=f"baseline {relative_text} sha256")
        source = confined_file(root, relative_text, field="baseline source path")
        if source.is_symlink() or not source.is_file():
            raise SequenceError(f"baseline source is missing or not a regular file: {relative_text}")
        actual_hash = digest(source)
        if actual_hash != expected_hash:
            raise SequenceError(f"baseline source hash mismatch for {relative_text}")
        destination = confined_file(worktree, relative_text, field="baseline destination")
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(source.read_bytes())
        copied.append({"path": relative_text, "sha256": actual_hash, "role": role})
    entrypoint = safe_relative_path(baseline.get("entrypoint"), field="baseline entrypoint").as_posix()
    if entrypoint not in seen:
        raise SequenceError("baseline entrypoint is not listed in source_manifest")
    return copied


def _stage_oracle_support(oracle: dict[str, Any], runtime_dir: Path, fixtures_root: Path) -> list[dict[str, str]]:
    files = oracle.get("support_files", [])
    if not isinstance(files, list):
        raise SequenceError("oracle support_files must be a list")
    copied: list[dict[str, str]] = []
    for raw in files:
        source = confined_file(fixtures_root, raw, field="oracle support file")
        if source.is_symlink() or not source.is_file():
            raise SequenceError(f"oracle support file is missing or not regular: {raw!r}")
        relative = safe_relative_path(raw, field="oracle support file")
        destination = confined_file(
            runtime_dir,
            (Path("support") / relative).as_posix(),
            field="oracle support destination",
        )
        contents = source.read_bytes()
        if destination.exists():
            if destination.is_symlink() or not destination.is_file() or destination.read_bytes() != contents:
                raise SequenceError(f"staged oracle support input conflicts with its frozen source: {raw}")
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(contents)
        copied.append({
            "path": f"support/{relative.as_posix()}",
            "source": str(raw),
            "sha256": sha256_bytes(contents),
        })
    return copied


def validate_sequence_prefix(sequence: dict[str, Any], prefix: list[str], *, checkpoint: str) -> list[dict[str, Any]]:
    declared_prefix = sequence.get("cumulative_regressions")
    if declared_prefix != prefix:
        raise SequenceError(
            f"{checkpoint} cumulative_regressions must be the exact sequence prefix {prefix}"
        )
    targets = sequence.get("regression_targets")
    if not isinstance(targets, list) or not targets:
        raise SequenceError(f"{checkpoint} has no executable cumulative regression targets")
    target_ids = [target.get("fixture") if isinstance(target, dict) else None for target in targets]
    if target_ids != prefix:
        raise SequenceError(
            f"{checkpoint} regression_targets must cover the exact sequence prefix {prefix} in order"
        )
    for target in targets:
        if not isinstance(target, dict):
            raise SequenceError(f"{checkpoint} regression target must be an object")
        safe_relative_path(target.get("entrypoint"), field=f"{checkpoint} regression entrypoint")
        oracle = target.get("oracle")
        if not isinstance(oracle, dict) or oracle.get("mode") not in {
            "mimi_stdout", "mimi_check_reject", "w1_scoped_cli",
            "w4_host_driver", "w4_endpoint_scope",
        }:
            raise SequenceError(f"{checkpoint} regression target has no supported oracle mode")
        if not isinstance(oracle.get("file"), str) or not isinstance(oracle.get("key"), str):
            raise SequenceError(f"{checkpoint} regression target oracle needs file and key")
        _verify_sha256(oracle.get("sha256"), field=f"{checkpoint} oracle sha256")
        paired = oracle.get("paired_mimi")
        if paired is not None and (
            not isinstance(paired, dict)
            or paired.get("mode") != "mimi_stdout"
            or not isinstance(paired.get("file"), str)
            or not isinstance(paired.get("key"), str)
        ):
            raise SequenceError(f"{checkpoint} paired_mimi must be a separate mimi_stdout oracle descriptor")
    return targets


def _load_oracle(oracle: dict[str, Any], fixtures_root: Path) -> dict[str, Any]:
    path = confined_file(fixtures_root, oracle.get("file"), field="oracle file")
    if path.is_symlink() or not path.is_file():
        raise SequenceError(f"oracle file is missing or not regular: {oracle.get('file')!r}")
    expected_hash = _verify_sha256(oracle.get("sha256"), field="oracle sha256")
    actual_hash = digest(path)
    if actual_hash != expected_hash:
        raise SequenceError(f"oracle file hash mismatch: {oracle.get('file')!r}")
    try:
        content = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SequenceError(f"cannot read oracle JSON {oracle.get('file')!r}: {error}") from error
    key = oracle["key"]
    if key in ("$", "/"):
        value = content
    elif key in content:
        value = content[key]
    else:
        value = content
        parts = key.strip("/").split("/") if "/" in key else key.split(".")
        for part in parts:
            if not part or not isinstance(value, dict) or part not in value:
                raise SequenceError(f"oracle key {key!r} is missing from {oracle['file']}")
            value = value[part]
    if not isinstance(value, dict):
        raise SequenceError(f"oracle value {key!r} must be a JSON object")
    return value


def _run_mimi_check_reject_target(
    record: dict[str, Any],
    *,
    target: dict[str, Any],
    runtime_dir: Path,
    entrypoint: Path,
    mimi: Path,
    expected: dict[str, Any],
    env: dict[str, str],
    timeout: float,
) -> dict[str, Any]:
    required = expected.get("diagnostic_contains")
    if isinstance(required, str):
        required = [required]
    if not isinstance(required, list) or not required or not all(isinstance(item, str) and item for item in required):
        record["status"] = "failed"
        record["detail"] = "mimi_check_reject oracle needs non-empty diagnostic_contains strings"
        return record
    command = [str(mimi), "check", str(entrypoint)]
    result = run_command(command, cwd=entrypoint.parent, env=env.copy(), timeout=timeout)
    combined = f"{result['stdout']}\n{result['stderr']}"
    record["check_command"] = command
    record["raw_logs"] = {"check": save_command_logs(runtime_dir, "check-reject", result)}
    record["check_returncode"] = result["returncode"]
    record["check_stdout"] = result["stdout"][-3000:]
    record["check_stderr"] = result["stderr"][-3000:]
    record["expected_diagnostics"] = required
    record["check"] = (
        "pass"
        if result["returncode"] not in (0, None)
        and not result["timed_out"]
        and all(item in combined for item in required)
        else "mismatch"
    )
    record["status"] = "pass" if record["check"] == "pass" else "failed"
    return record


def _run_w4_endpoint_scope_target(
    record: dict[str, Any],
    *,
    runtime_dir: Path,
    expected: dict[str, Any],
    cc: str,
    env: dict[str, str],
    timeout: float,
) -> dict[str, Any]:
    expected_stdout = expected.get("stdout")
    if not isinstance(expected_stdout, str):
        record["status"] = "failed"
        record["detail"] = "w4_endpoint_scope oracle needs exact stdout"
        return record
    staged = {
        Path(item["path"]).name: runtime_dir / item["path"]
        for item in record.get("support_files", [])
        if isinstance(item, dict) and isinstance(item.get("path"), str)
    }
    required_names = ("w4_trusted.c", "w4_endpoint_scope_s3.c", "endpoint_scope_oracle.c")
    missing = [name for name in required_names if name not in staged or not staged[name].is_file()]
    if missing:
        record["status"] = "failed"
        record["detail"] = f"w4_endpoint_scope support inputs are missing: {missing}"
        return record
    binary = runtime_dir / "w4-endpoint-scope-oracle"
    command = [
        cc,
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-Wl,--wrap=socket,--wrap=bind,--wrap=connect,--wrap=listen",
        *(str(staged[name]) for name in required_names),
        "-o",
        str(binary),
    ]
    build = run_command(command, cwd=runtime_dir, env=env, timeout=max(timeout, 60.0))
    record["endpoint_build_command"] = command
    record.setdefault("raw_logs", {})["endpoint_build"] = save_command_logs(runtime_dir, "endpoint-build", build)
    if build["returncode"] != 0 or not binary.is_file():
        record["endpoint_oracle"] = "build_failed"
        record["detail"] = (build["stderr"] or build["stdout"] or "endpoint oracle build failed")[-3000:]
        record["status"] = "failed"
        return record
    run = run_command([str(binary)], cwd=runtime_dir, env=env, timeout=timeout)
    record["endpoint_command"] = [str(binary)]
    record["raw_logs"]["endpoint_run"] = save_command_logs(runtime_dir, "endpoint-run", run)
    record["endpoint_stdout"] = run["stdout"]
    record["endpoint_stderr"] = run["stderr"][-3000:]
    endpoint_pass = (
        run["returncode"] == 0
        and not run["timed_out"]
        and run["stdout"] == expected_stdout
    )
    record["endpoint_oracle"] = "pass" if endpoint_pass else "timeout" if run["timed_out"] else "mismatch"
    record["endpoint_oracle_expected_stdout"] = expected_stdout
    record["endpoint_oracle_sha256"] = digest(binary)

    if "mimi_stdout" in expected:
        record["status"] = "failed"
        record["detail"] = "keep Mimi stdout and endpoint syscall oracles in separate regression targets"
        return record
    record["status"] = "pass" if endpoint_pass else "failed"
    return record


def _run_sequence_target(
    target: dict[str, Any],
    *,
    worktree: Path,
    runtime_dir: Path,
    mimi: Path,
    timeout: float,
    env: dict[str, str],
    fixtures_root: Path,
    cc: str,
) -> dict[str, Any]:
    fixture_id = target["fixture"]
    oracle = target["oracle"]
    mode = oracle["mode"]
    target_runtime = runtime_dir / fixture_id
    target_runtime.mkdir()
    record: dict[str, Any] = {"fixture": fixture_id, "mode": mode, "status": "unverified"}
    entrypoint = confined_file(worktree, target["entrypoint"], field="regression entrypoint")
    record["entrypoint"] = target["entrypoint"]
    if not entrypoint.is_file():
        record["status"] = "failed"
        record["detail"] = f"cumulative entrypoint missing: {target['entrypoint']}"
        return record
    try:
        expected = _load_oracle(oracle, fixtures_root)
    except SequenceError as error:
        record["status"] = "failed"
        record["detail"] = str(error)
        return record
    try:
        support_copies = _stage_oracle_support(oracle, target_runtime, fixtures_root)
    except SequenceError as error:
        record["status"] = "failed"
        record["detail"] = str(error)
        return record
    record["support_files"] = support_copies
    if mode == "w1_scoped_cli":
        record["status"] = "unsupported"
        record["detail"] = "scoped CLI grant injection is not implemented by this evaluator"
        return record
    if mode == "w4_host_driver":
        return _run_w4_host_target(
            record,
            target=target,
            worktree=worktree,
            runtime_dir=target_runtime,
            entrypoint=entrypoint,
            mimi=mimi,
            timeout=timeout,
            env=env,
            fixtures_root=fixtures_root,
            expected=expected,
            cc=cc,
        )
    if mode == "w4_endpoint_scope":
        endpoint_record = _run_w4_endpoint_scope_target(
            record,
            runtime_dir=target_runtime,
            expected=expected,
            cc=cc,
            env=env,
            timeout=timeout,
        )
        paired = oracle.get("paired_mimi")
        if paired is not None:
            if not isinstance(paired, dict) or paired.get("mode") != "mimi_stdout":
                endpoint_record["status"] = "failed"
                endpoint_record["detail"] = "paired_mimi must use a separate mimi_stdout oracle"
                return endpoint_record
            try:
                _load_oracle(paired, fixtures_root)
            except SequenceError as error:
                endpoint_record["status"] = "failed"
                endpoint_record["detail"] = f"paired Mimi oracle is invalid: {error}"
                return endpoint_record
            paired_runtime = target_runtime / "paired-runtime"
            paired_runtime.mkdir()
            paired_result = _run_sequence_target(
                {"fixture": fixture_id, "entrypoint": target["entrypoint"], "oracle": paired},
                worktree=worktree,
                runtime_dir=paired_runtime,
                mimi=mimi,
                timeout=timeout,
                env=env,
                fixtures_root=fixtures_root,
                cc=cc,
            )
            endpoint_record["paired_mimi"] = paired_result
            endpoint_record["status"] = (
                "pass"
                if endpoint_record.get("endpoint_oracle") == "pass" and paired_result.get("status") == "pass"
                else "failed"
            )
        return endpoint_record
    if mode == "mimi_check_reject":
        return _run_mimi_check_reject_target(
            record,
            target=target,
            runtime_dir=target_runtime,
            entrypoint=entrypoint,
            mimi=mimi,
            expected=expected,
            env=env,
            timeout=timeout,
        )
    if mode != "mimi_stdout":
        record["status"] = "unsupported"
        record["detail"] = f"oracle mode {mode!r} is not implemented by this evaluator"
        return record
    expected_stdout = expected.get("stdout")
    if not isinstance(expected_stdout, str):
        record["status"] = "failed"
        record["detail"] = "mimi_stdout oracle has no string stdout"
        return record
    expected_files = expected.get("files", {})
    if not isinstance(expected_files, dict) or not all(
        isinstance(path, str) and isinstance(contents, str) for path, contents in expected_files.items()
    ):
        record["status"] = "failed"
        record["detail"] = "mimi_stdout oracle files must map relative paths to text"
        return record

    target_env = env.copy()
    cwd = entrypoint.parent
    check = run_command([str(mimi), "check", str(entrypoint)], cwd=cwd, env=target_env, timeout=timeout)
    record["check_command"] = [str(mimi), "check", str(entrypoint)]
    record.setdefault("raw_logs", {})["check"] = save_command_logs(target_runtime, "check", check)
    record["check"] = "pass" if check["returncode"] == 0 else "fail"
    record["check_diagnostics"] = (check["stderr"] or check["stdout"])[-2000:]
    if record["check"] != "pass":
        record["status"] = "failed"
        return record
    vm = run_command([str(mimi), "run", str(entrypoint)], cwd=cwd, env=target_env, timeout=timeout)
    record["vm_command"] = [str(mimi), "run", str(entrypoint)]
    record["raw_logs"]["vm"] = save_command_logs(target_runtime, "vm", vm)
    record["vm_stdout"] = vm["stdout"]
    record["vm"] = (
        "timeout" if vm["timed_out"] else
        "failed" if vm["returncode"] != 0 else
        "pass" if vm["stdout"] == expected_stdout else "mismatch"
    )
    if record["vm"] == "pass":
        file_check = _check_expected_files(cwd, expected_files)
        record["vm_files"] = file_check
        if file_check != "pass":
            record["vm"] = "file_mismatch"
    record["vm_stderr"] = vm["stderr"][-2000:]
    if record["vm"] != "pass":
        record["status"] = "failed"
        return record
    executable = target_runtime / f"sequence-{fixture_id.lower()}"
    build = run_command([str(mimi), "build", str(entrypoint), "-o", str(executable)], cwd=cwd, env=target_env, timeout=max(timeout, 120.0))
    record["build_command"] = [str(mimi), "build", str(entrypoint), "-o", str(executable)]
    record["raw_logs"]["build"] = save_command_logs(target_runtime, "build", build)
    if build["returncode"] != 0 or not executable.is_file():
        record["native"] = "unsupported" if unsupported_diagnostic(f"{build['stdout']}\n{build['stderr']}") else "build_failed"
        record["native_diagnostics"] = (build["stderr"] or build["stdout"])[-2000:]
        record["status"] = "failed"
        return record
    native = run_command([str(executable)], cwd=cwd, env=target_env, timeout=timeout)
    record["native_command"] = [str(executable)]
    record["raw_logs"]["native"] = save_command_logs(target_runtime, "native", native)
    record["native_stdout"] = native["stdout"]
    record["native"] = (
        "timeout" if native["timed_out"] else
        "failed" if native["returncode"] != 0 else
        "pass" if native["stdout"] == expected_stdout else "mismatch"
    )
    if record["native"] == "pass":
        file_check = _check_expected_files(cwd, expected_files)
        record["native_files"] = file_check
        if file_check != "pass":
            record["native"] = "file_mismatch"
    record["native_stderr"] = native["stderr"][-2000:]
    record["status"] = "pass" if record["native"] == "pass" else "failed"
    return record


def _check_expected_files(directory: Path, expected_files: dict[str, str]) -> str:
    for raw_path, contents in expected_files.items():
        try:
            output = confined_file(directory, raw_path, field="expected output file")
        except SequenceError:
            return "invalid_path"
        if output.is_symlink() or not output.is_file():
            return "missing"
        try:
            actual = output.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return "unreadable"
        if actual != contents:
            return "mismatch"
    return "pass"


def _run_sequence_target_tracked(
    target: dict[str, Any],
    *,
    worktree: Path,
    runtime_dir: Path,
    mimi: Path,
    timeout: float,
    env: dict[str, str],
    fixtures_root: Path,
    cc: str,
) -> dict[str, Any]:
    before = tree_file_hashes(worktree)
    result = _run_sequence_target(
        target,
        worktree=worktree,
        runtime_dir=runtime_dir,
        mimi=mimi,
        timeout=timeout,
        env=env,
        fixtures_root=fixtures_root,
        cc=cc,
    )
    after = tree_file_hashes(worktree)
    _record_cumulative_mutations(result, before=before, after=after)
    return result


def _find_w4_file(worktree: Path, basename: str) -> Path:
    matches = [path for path in worktree.rglob(basename) if path.is_file() and not path.is_symlink()]
    if not matches:
        raise SequenceError(f"W4 cumulative worktree is missing {basename}")
    matches.sort(key=lambda path: (len(path.relative_to(worktree).parts), path.relative_to(worktree).as_posix()))
    if len(matches) > 1 and matches[0].read_bytes() != matches[1].read_bytes():
        # The explicit source tree copy wins over its evidence-support copy;
        # if equal-depth candidates diverge, the target is ambiguous.
        shortest = len(matches[0].relative_to(worktree).parts)
        same_depth = [path for path in matches if len(path.relative_to(worktree).parts) == shortest]
        if len({path.read_bytes() for path in same_depth}) > 1:
            raise SequenceError(f"multiple cumulative versions of W4 {basename} are ambiguous")
    return matches[0]


def _run_w4_host_target(
    record: dict[str, Any],
    *,
    target: dict[str, Any],
    worktree: Path,
    runtime_dir: Path,
    entrypoint: Path,
    mimi: Path,
    timeout: float,
    env: dict[str, str],
    fixtures_root: Path,
    expected: dict[str, Any],
    cc: str,
) -> dict[str, Any]:
    required = ("checksum", "clients", "stop", "exports", "limits")
    missing = [key for key in required if key not in expected]
    if missing:
        record["status"] = "failed"
        record["detail"] = f"W4 host oracle is missing required sections: {missing}"
        return record
    host_driver_candidates = [
        runtime_dir / item["path"]
        for item in record.get("support_files", [])
        if isinstance(item, dict)
        and isinstance(item.get("source"), str)
        and PurePosixPath(item["source"]).name == "w4_host_driver.c"
    ]
    if len(host_driver_candidates) != 1:
        record["status"] = "failed"
        record["detail"] = "W4 host oracle must stage exactly one w4_host_driver.c support input"
        return record
    host_driver = host_driver_candidates[0]
    if host_driver.is_symlink() or not host_driver.is_file():
        record["status"] = "failed"
        record["detail"] = "staged W4 host driver is missing or not regular"
        return record
    source_app_root = entrypoint.parent.parent
    app_relative_entrypoint = entrypoint.relative_to(source_app_root)
    app_root = runtime_dir / "app"
    try:
        shutil.copytree(source_app_root, app_root)
    except OSError as error:
        record["status"] = "failed"
        record["detail"] = f"cannot create isolated W4 execution copy: {error}"
        return record
    entrypoint = app_root / app_relative_entrypoint
    trusted_source = app_root / "native" / "w4_trusted.c"
    if trusted_source.is_symlink() or not trusted_source.is_file():
        record["status"] = "failed"
        record["detail"] = f"cumulative W4 trusted source is missing: {trusted_source.relative_to(worktree)}"
        return record
    record["source_entrypoint"] = str(app_relative_entrypoint.as_posix())
    record["execution_root"] = str(app_root.relative_to(runtime_dir))
    record["host_driver_source"] = str(host_driver.relative_to(runtime_dir))
    record["trusted_source"] = str(trusted_source.relative_to(app_root))
    if (app_root / "mimi.toml").is_file() and (app_root / "mimi.lock").is_file():
        install = run_command(
            [str(mimi), "install", "--offline", "--frozen"],
            cwd=app_root,
            env=env,
            timeout=max(timeout, 120.0),
        )
        record.setdefault("raw_logs", {})["install"] = save_command_logs(runtime_dir, "install", install)
        record["install"] = "pass" if install["returncode"] == 0 else "fail"
        record["install_stdout"] = install["stdout"][-2000:]
        record["install_stderr"] = install["stderr"][-2000:]
        if record["install"] != "pass":
            record["status"] = "failed"
            return record

    exports_source = entrypoint.parent / "exports.mimi"
    if not exports_source.is_file():
        record["status"] = "failed"
        record["detail"] = f"W4 export source is missing beside the entrypoint: {exports_source}"
        return record
    trusted_library = runtime_dir / "libw4_trusted.so"
    exports_library = runtime_dir / "libw4_exports.so"
    driver = runtime_dir / "w4_host_driver"
    trusted_build = run_command(
        [cc, "-std=c11", "-Wall", "-Wextra", "-Werror", "-fPIC", "-shared", str(trusted_source), "-o", str(trusted_library)],
        cwd=runtime_dir,
        env=env,
        timeout=60,
    )
    record.setdefault("raw_logs", {})["trusted_build"] = save_command_logs(runtime_dir, "trusted-build", trusted_build)
    if trusted_build["returncode"] != 0 or not trusted_library.is_file():
        record["status"] = "failed"
        record["detail"] = (trusted_build["stderr"] or trusted_build["stdout"] or "trusted C build failed")[-2000:]
        return record
    driver_build = run_command(
        [cc, "-std=c11", "-Wall", "-Wextra", "-Werror", "-pthread", "-ldl", str(host_driver), "-o", str(driver)],
        cwd=runtime_dir,
        env=env,
        timeout=60,
    )
    record["raw_logs"]["driver_build"] = save_command_logs(runtime_dir, "driver-build", driver_build)
    if driver_build["returncode"] != 0 or not driver.is_file():
        record["status"] = "failed"
        record["detail"] = (driver_build["stderr"] or driver_build["stdout"] or "W4 host driver build failed")[-2000:]
        return record
    exports_build = run_command(
        [str(mimi), "build", str(exports_source), "--shared", "-o", str(exports_library)],
        cwd=app_root,
        env=env,
        timeout=max(timeout, 120.0),
    )
    record["raw_logs"]["exports_build"] = save_command_logs(runtime_dir, "exports-build", exports_build)
    record["exports_build"] = "pass" if exports_build["returncode"] == 0 and exports_library.is_file() else "fail"
    record["exports_build_diagnostics"] = (exports_build["stderr"] or exports_build["stdout"])[-2000:]
    if record["exports_build"] != "pass":
        record["status"] = "failed"
        return record

    check = run_command([str(mimi), "check", str(entrypoint)], cwd=app_root, env=env, timeout=timeout)
    record["check_command"] = [str(mimi), "check", str(entrypoint)]
    record["raw_logs"]["check"] = save_command_logs(runtime_dir, "check", check)
    record["check"] = "pass" if check["returncode"] == 0 else "fail"
    record["check_diagnostics"] = (check["stderr"] or check["stdout"])[-2000:]
    if record["check"] != "pass":
        record["status"] = "failed"
        return record

    native_env = env.copy()
    native_env["LD_LIBRARY_PATH"] = f"{runtime_dir}:{native_env.get('LD_LIBRARY_PATH', '')}"
    native_binary = runtime_dir / "w4_service"
    native_build = run_command(
        [str(mimi), "build", str(entrypoint), "--link-search", str(runtime_dir), "--link-lib", "w4_trusted", "-o", str(native_binary)],
        cwd=app_root,
        env=native_env,
        timeout=max(timeout, 120.0),
    )
    record["native_build_command"] = [str(mimi), "build", str(entrypoint), "--link-search", str(runtime_dir), "--link-lib", "w4_trusted", "-o", str(native_binary)]
    record["raw_logs"]["native_build"] = save_command_logs(runtime_dir, "native-build", native_build)
    if native_build["returncode"] != 0 or not native_binary.is_file():
        record["native"] = "unsupported" if unsupported_diagnostic(f"{native_build['stdout']}\n{native_build['stderr']}") else "build_failed"
        record["native_diagnostics"] = (native_build["stderr"] or native_build["stdout"])[-2000:]
        record["status"] = "failed"
        return record

    vm_command = [str(driver), "--vm", str(mimi), str(entrypoint), str(exports_library), str(trusted_library)]
    vm = run_command(vm_command, cwd=runtime_dir, env=native_env, timeout=max(timeout, 120.0))
    native = run_command(
        [str(driver), "--native", str(native_binary), str(exports_library), str(trusted_library)],
        cwd=runtime_dir,
        env=native_env,
        timeout=max(timeout, 120.0),
    )
    record["vm_command"] = vm_command
    record["native_command"] = [str(driver), "--native", str(native_binary), str(exports_library), str(trusted_library)]
    record["raw_logs"]["vm"] = save_command_logs(runtime_dir, "vm-host-driver", vm)
    record["raw_logs"]["native"] = save_command_logs(runtime_dir, "native-host-driver", native)
    for mode, result in (("vm", vm), ("native", native)):
        text = f"{result['stdout']}\n{result['stderr']}"
        markers = ("C_ORACLE_OK", "W4_CYCLE_OK", "W4_METRICS")
        passed = result["returncode"] == 0 and not result["timed_out"] and all(marker in text for marker in markers)
        record[mode] = "pass" if passed else "timeout" if result["timed_out"] else "failed"
        record[f"{mode}_stdout"] = result["stdout"][-3000:]
        record[f"{mode}_stderr"] = result["stderr"][-3000:]
    record["native_sha256"] = digest(native_binary)
    record["exports_sha256"] = digest(exports_library)
    record["trusted_sha256"] = digest(trusted_library)
    record["host_driver_sha256"] = digest(driver)
    record["status"] = "pass" if record["vm"] == "pass" and record["native"] == "pass" else "failed"
    return record


def run_sequence(
    sequence: dict[str, Any],
    *,
    mimi: Path,
    timeout: float,
    fixtures_root: Path,
    artifact_root: Path,
    cc: str,
) -> dict[str, Any]:
    sequence_id = sequence.get("id")
    if sequence_id not in EXPECTED_SEQUENCES:
        raise SequenceError(f"unknown required sequence id: {sequence_id!r}")
    expected_ids = EXPECTED_SEQUENCES[sequence_id]
    if sequence.get("execution") != "cumulative_patch_chain":
        raise SequenceError(f"{sequence_id} must declare execution=cumulative_patch_chain")
    baseline = sequence.get("baseline")
    if not isinstance(baseline, dict):
        raise SequenceError(f"{sequence_id} baseline must be an object")
    expected_baseline = EXPECTED_BASELINES[sequence_id]
    if baseline.get("root") != expected_baseline["root"] or baseline.get("entrypoint") != expected_baseline["entrypoint"]:
        raise SequenceError(f"{sequence_id} baseline root/entrypoint does not match the frozen sequence contract")
    steps = sequence.get("steps")
    if not isinstance(steps, list):
        raise SequenceError(f"{sequence_id} steps must be an ordered list")
    declared_ids = [step.get("fixture") if isinstance(step, dict) else None for step in steps]
    if [baseline.get("fixture"), *declared_ids] != expected_ids:
        raise SequenceError(f"{sequence_id} has missing, extra, or reordered checkpoints")
    declared_blockers = sequence.get("blockers", [])
    if not isinstance(declared_blockers, list):
        raise SequenceError(f"{sequence_id} blockers must be a list")
    chain_root = artifact_root / sequence_id
    chain_root.mkdir(parents=True, exist_ok=False)
    worktree = chain_root / "worktree"
    worktree.mkdir()
    runtime_dir = chain_root / "runtime"
    runtime_dir.mkdir()
    copied_baseline = _copy_baseline(sequence, worktree, fixtures_root)
    base_prefix = [expected_ids[0]]
    baseline_contract_error: str | None = None
    try:
        baseline_targets = validate_sequence_prefix(
            baseline, base_prefix, checkpoint=f"{sequence_id}/{expected_ids[0]}"
        )
    except SequenceError as error:
        baseline_targets = []
        baseline_contract_error = str(error)

    output_hash = tree_digest(worktree)
    baseline_row: dict[str, Any] = {
        "fixture": expected_ids[0],
        "checkpoint": expected_ids[0],
        "status": "blocked" if baseline_contract_error else "ready",
        "parent_hash": None,
        "initial_hash": output_hash,
        "output_hash": output_hash,
        "source_manifest": copied_baseline,
        "regressions": [],
    }
    if baseline_contract_error:
        baseline_row["detail"] = baseline_contract_error
    elif baseline.get("status") not in (None, "ready"):
        baseline_row["detail"] = f"baseline is declared {baseline.get('status')!r}"
        baseline_row["status"] = "blocked"
    else:
        baseline_runtime = runtime_dir / f"00-{expected_ids[0]}"
        baseline_runtime.mkdir()
        for target in baseline_targets:
            try:
                baseline_row["regressions"].append(_run_sequence_target_tracked(
                    target,
                    worktree=worktree,
                    runtime_dir=baseline_runtime,
                    mimi=mimi,
                    timeout=timeout,
                    env={**os.environ, "LLVM_SYS_181_PREFIX": os.environ.get("LLVM_SYS_181_PREFIX", "/tmp/llvm-wrapper")},
                    fixtures_root=fixtures_root,
                    cc=cc,
                ))
            except SequenceError as error:
                baseline_row["regressions"].append({
                    "fixture": target.get("fixture"),
                    "status": "failed",
                    "detail": str(error),
                })
        if any(item.get("status") != "pass" for item in baseline_row["regressions"]):
            baseline_row["status"] = (
                "blocked"
                if any(item.get("status") in ("unsupported", "blocked", "unverified") for item in baseline_row["regressions"])
                else "failed"
            )
        else:
            baseline_row["status"] = "pass"

    output_hash = tree_digest(worktree)
    baseline_row["output_hash"] = output_hash
    baseline_snapshot = chain_root / "checkpoints" / f"00-{expected_ids[0]}" / "worktree"
    baseline_snapshot.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(worktree, baseline_snapshot)
    baseline_row["worktree"] = str(baseline_snapshot.relative_to(artifact_root))

    rows = [baseline_row]
    prefix = base_prefix
    previous_hash = output_hash
    # Empty/unwired regression maps block acceptance but do not erase the
    # declared source evolution. A real execution mismatch or patch-integrity
    # error stops the chain immediately.
    source_stopped = baseline_row["status"] == "failed"
    for offset, step in enumerate(steps, start=1):
        fixture_id = step["fixture"]
        prefix = [*prefix, fixture_id]
        row: dict[str, Any] = {
            "fixture": fixture_id,
            "checkpoint": step.get("checkpoint", fixture_id),
            "parent_hash": previous_hash,
            "output_hash": None,
            "source_patch": None,
            "status": "blocked" if source_stopped else "unverified",
            "cumulative_regressions": step.get("cumulative_regressions"),
            "regressions": [],
        }
        anchor = step.get("anchor")
        try:
            anchor_path = confined_file(fixtures_root, anchor, field=f"{fixture_id} anchor")
            if anchor_path.is_symlink() or not anchor_path.is_file():
                raise SequenceError(f"isolated anchor is missing or not regular: {anchor!r}")
            row["anchor"] = str(anchor)
            row["anchor_sha256"] = digest(anchor_path)
            if source_stopped:
                row["detail"] = "a prior checkpoint failed; no later patch was applied"
                rows.append(row)
                continue

            target_contract_error: str | None = None
            try:
                targets = validate_sequence_prefix(step, prefix, checkpoint=f"{sequence_id}/{fixture_id}")
            except SequenceError as error:
                targets = []
                target_contract_error = str(error)
            if step.get("status") not in (None, "ready"):
                detail = f"{fixture_id} is declared {step.get('status')!r}, not ready"
                target_contract_error = f"{target_contract_error}; {detail}" if target_contract_error else detail

            patch = step.get("patch")
            if not isinstance(patch, dict):
                raise SequenceError(f"{fixture_id} has no declared cumulative patch")
            expected_patch_path = EXPECTED_PATCH_PATHS[sequence_id][fixture_id]
            if patch.get("path") != expected_patch_path:
                raise SequenceError(
                    f"{fixture_id} patch path must be the ordered artifact {expected_patch_path!r}"
                )
            if patch.get("format") != "unified_diff":
                raise SequenceError(f"{fixture_id} patch format must be unified_diff")
            patch_path = confined_file(fixtures_root, patch.get("path"), field=f"{fixture_id} patch path")
            if patch_path.is_symlink() or not patch_path.is_file():
                raise SequenceError(f"{fixture_id} patch file is missing or not regular")
            expected_patch_hash = _verify_sha256(patch.get("sha256"), field=f"{fixture_id} patch sha256")
            patch_bytes = patch_path.read_bytes()
            actual_patch_hash = sha256_bytes(patch_bytes)
            if actual_patch_hash != expected_patch_hash:
                raise SequenceError(f"{fixture_id} patch hash mismatch")
            applied = apply_unified_diff(worktree, patch_bytes)
            entrypoint_text = step.get("entrypoint")
            if not isinstance(entrypoint_text, str):
                raise SequenceError(f"{fixture_id} has no current cumulative entrypoint")
            expected_entrypoint = (
                "offline-release/src/main.mimi"
                if sequence_id == "SEQ-W4" and fixture_id == "S4"
                else EXPECTED_BASELINES[sequence_id]["entrypoint"]
            )
            if entrypoint_text != expected_entrypoint:
                raise SequenceError(f"{fixture_id} entrypoint does not match the frozen sequence contract")
            entrypoint_path = confined_file(worktree, entrypoint_text, field=f"{fixture_id} entrypoint")
            patch_output_hash = tree_digest(worktree)
            row["source_patch"] = {
                "path": str(patch.get("path")),
                "sha256": actual_patch_hash,
                "changes": applied,
            }
            row["entrypoint"] = entrypoint_text
            row["patch_output_hash"] = patch_output_hash
            if not entrypoint_path.is_file():
                row["output_hash"] = patch_output_hash
                previous_hash = patch_output_hash
                checkpoint_dir = chain_root / "checkpoints" / f"{offset:02d}-{fixture_id}"
                checkpoint_worktree = checkpoint_dir / "worktree"
                checkpoint_dir.mkdir(parents=True, exist_ok=False)
                shutil.copytree(worktree, checkpoint_worktree)
                row["worktree"] = str(checkpoint_worktree.relative_to(artifact_root))
                raise SequenceError(f"{fixture_id} cumulative entrypoint is missing after patch: {entrypoint_text}")

            if target_contract_error:
                row["detail"] = target_contract_error
                row["status"] = "blocked"
            else:
                checkpoint_runtime = runtime_dir / f"{offset:02d}-{fixture_id}"
                checkpoint_runtime.mkdir()
                for target in targets:
                    try:
                        row["regressions"].append(_run_sequence_target_tracked(
                            target,
                            worktree=worktree,
                            runtime_dir=checkpoint_runtime,
                            mimi=mimi,
                            timeout=timeout,
                            env={**os.environ, "LLVM_SYS_181_PREFIX": os.environ.get("LLVM_SYS_181_PREFIX", "/tmp/llvm-wrapper")},
                            fixtures_root=fixtures_root,
                            cc=cc,
                        ))
                    except SequenceError as error:
                        row["regressions"].append({
                            "fixture": target.get("fixture"),
                            "status": "failed",
                            "detail": str(error),
                        })
                if any(item.get("status") != "pass" for item in row["regressions"]):
                    row["status"] = (
                        "blocked"
                        if any(item.get("status") in ("unsupported", "blocked", "unverified") for item in row["regressions"])
                        else "failed"
                    )
                    if row["status"] == "failed":
                        source_stopped = True
                else:
                    row["status"] = "pass"
            state_hash = tree_digest(worktree)
            row["output_hash"] = state_hash
            previous_hash = state_hash
            checkpoint_dir = chain_root / "checkpoints" / f"{offset:02d}-{fixture_id}"
            checkpoint_worktree = checkpoint_dir / "worktree"
            checkpoint_dir.mkdir(parents=True, exist_ok=False)
            shutil.copytree(worktree, checkpoint_worktree)
            row["worktree"] = str(checkpoint_worktree.relative_to(artifact_root))
        except SequenceError as error:
            row["status"] = "blocked" if "no declared cumulative patch" in str(error) else "failed"
            row["detail"] = str(error)
            source_stopped = True
        rows.append(row)

    all_rows_pass = bool(rows) and all(row.get("status") == "pass" for row in rows)
    declared_ready = sequence.get("status") == "ready" and not declared_blockers
    if all_rows_pass and declared_ready:
        overall_status = "pass"
    elif any(row.get("status") == "failed" for row in rows):
        overall_status = "failed"
    else:
        overall_status = "blocked"
    result = {
        "id": sequence_id,
        "execution": sequence.get("execution"),
        "status": overall_status,
        "accepted": False,
        "artifacts": str(chain_root.relative_to(artifact_root)),
        "checkpoints": rows,
        "blockers": declared_blockers,
        "declared_status": sequence.get("status"),
    }
    (chain_root / "sequence.json").write_text(
        json.dumps(result, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return result

def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def reference(case_id: str) -> tuple[str, dict[str, str]]:
    """Independent requirement oracle (does not inspect Mimi output)."""

    if case_id == "W1":
        return "demo:3\n", {"v02-w1-report.json": "demo:3"}
    if case_id == "W2":
        return "20\n10\n10\n0\n", {}
    if case_id == "W2_FLOW":
        return "5\n", {}
    if case_id == "W3":
        values = [-2147483648, -501, 0, 501, 2147483647]
        clipped = [max(-1000, min(1000, value * 2)) for value in values]
        return "".join(f"{value}\n" for value in clipped), {}
    if case_id == "W4":
        return f"{2 * 20 + 2 * 21 + 1}\n", {}
    if case_id == "W4_CONCURRENCY":
        return f"{2 * 20 + 2 * 21}\n", {}
    if case_id == "W4_FAULT":
        return "BadJson\n5\n", {}
    if case_id == "W4_NETWORK":
        return "BindOk\n", {}
    if case_id == "C1":
        return "demo-v2:3\n", {}
    if case_id == "C2":
        return "source=primary;name=demo;retries=3\n", {}
    if case_id == "C3":
        return "PermissionDenied\n", {}
    if case_id == "C4":
        return "preview=demo:3\n", {}
    if case_id == "D1":
        return "20\n10\n7\n", {}
    if case_id == "D2":
        return "Cancelled\nRetryable\n", {}
    if case_id == "D3":
        return "ChecksumMismatch\n", {}
    if case_id == "D4":
        return "20\n", {}
    if case_id == "D5":
        return "50\n", {}
    if case_id == "H1":
        return reference("W3")
    if case_id == "H2":
        return "-1000\n501\n", {}
    if case_id == "H3":
        return "30\n32\n", {}
    if case_id == "S1":
        return f"{2 * 20 + 2 * 21 + 1}\n", {}
    if case_id == "S2":
        return "Timeout\nRetryOwner\n", {}
    if case_id == "S3":
        return "EndpointDenied\n", {}
    if case_id == "S4":
        return f"{2 * 20 + 2 * 21 + 1}\n", {}
    raise KeyError(case_id)


def run_command(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout: float,
) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return {
            "returncode": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as error:
        return {
            "returncode": None,
            "stdout": error.stdout or "",
            "stderr": error.stderr or "",
            "timed_out": True,
        }
    except OSError as error:
        return {"returncode": None, "stdout": "", "stderr": str(error), "timed_out": False}


def save_command_logs(directory: Path, label: str, result: dict[str, Any]) -> dict[str, Any]:
    logs: dict[str, Any] = {}
    for stream in ("stdout", "stderr"):
        contents = str(result.get(stream, "")).encode("utf-8", errors="replace")
        path = directory / f"{label}.{stream}.log"
        path.write_bytes(contents)
        logs[stream] = {"path": str(path), "sha256": sha256_bytes(contents), "bytes": len(contents)}
    return logs


def unsupported_diagnostic(text: str) -> bool:
    lowered = text.lower()
    markers = (
        "mir-coverage-001",
        "mir-materialization-001",
        "canonical mir",
        "codegenlegacyremainder",
        "e0722",
        "not eligible for the default route",
        "rust runtime compilation failed",
        "legacy route",
    )
    return any(marker in lowered for marker in markers)


def compile_ffi(cc: str, directory: Path) -> tuple[Path | None, str | None]:
    source = FIXTURES / "oracle" / "v02_ffi.c"
    library = directory / "libv02ffi.so"
    result = run_command(
        [cc, "-shared", "-fPIC", str(source), "-o", str(library)],
        cwd=directory,
        env=os.environ.copy(),
        timeout=30,
    )
    if result["returncode"] != 0 or not library.exists():
        return None, (result["stderr"] or result["stdout"] or "C compiler failed")[-2000:]
    return library, None


def compile_detector(cc: str, sanitizer: str, directory: Path) -> tuple[Path | None, str]:
    source = FIXTURES / "oracle" / "native_fault_control.c"
    binary = directory / f"fault_control_{sanitizer}"
    flags = [cc, "-g", "-O1", "-fno-omit-frame-pointer", f"-fsanitize={sanitizer}", str(source), "-o", str(binary)]
    if sanitizer == "thread":
        flags.insert(1, "-pthread")
        flags.append("-pthread")
    result = run_command(flags, cwd=directory, env=os.environ.copy(), timeout=60)
    if result["returncode"] != 0 or not binary.exists():
        return None, (result["stderr"] or result["stdout"] or "sanitizer compiler failed")[-2000:]
    return binary, ""


def detector_case(cc: str, directory: Path) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for sanitizer, argument, marker in (
        ("address", "--uaf", "addresssanitizer"),
        ("thread", "--race", "threadsanitizer"),
    ):
        binary, detail = compile_detector(cc, sanitizer, directory)
        if binary is None:
            result[sanitizer] = {"status": "environment_unverified", "detail": detail}
            continue
        run = run_command([str(binary), argument], cwd=directory, env=os.environ.copy(), timeout=30)
        combined = f"{run['stdout']}\n{run['stderr']}".lower()
        # A non-zero exit by itself is not evidence that the sanitizer fired:
        # a broken control binary, loader failure, or unrelated crash would
        # otherwise be counted as a detector pass.  Require the sanitizer's
        # own diagnostic marker; retain other outcomes as an explicit failure
        # class so A-11 cannot be satisfied by an unexplained crash.
        if marker in combined:
            status = "detected"
            detail = None
        elif run["timed_out"]:
            status = "environment_unverified"
            detail = "fault control sample timed out before a sanitizer diagnostic"
        elif run["returncode"] not in (0, None):
            status = "environment_unverified"
            detail = (
                f"fault control exited {run['returncode']} without the "
                f"{marker} diagnostic marker"
            )
        else:
            status = "missed"
            detail = f"fault control exited cleanly without the {marker} diagnostic marker"
        result[sanitizer] = {
            "status": status,
            "returncode": run["returncode"],
            "output": combined[-2000:],
            **({"detail": detail} if detail else {}),
        }
    return result


def selected_entries(manifest: dict[str, Any], filters: set[str] | None) -> list[dict[str, Any]]:
    entries = [*manifest["apps"], *manifest["changes"]]
    if filters:
        return [entry for entry in entries if entry["id"] in filters]
    return entries


def check_expected_json(oracle: dict[str, Any]) -> list[str]:
    problems: list[str] = []
    for case_id in oracle:
        try:
            actual, files = reference(case_id)
        except KeyError:
            problems.append(f"oracle has no independent implementation for {case_id}")
            continue
        frozen = oracle[case_id]
        if frozen.get("stdout") != actual:
            problems.append(f"frozen stdout drift for {case_id}")
        if frozen.get("files", {}) != files:
            problems.append(f"frozen file oracle drift for {case_id}")
    return problems


def expected_for_entry(oracle: Any) -> tuple[str, dict[str, str]]:
    if isinstance(oracle, str):
        return reference(oracle)
    if not isinstance(oracle, dict):
        raise SequenceError("fixture oracle must be a key or an oracle object")
    if oracle.get("mode") != "mimi_stdout":
        raise SequenceError(f"normal app runner does not execute oracle mode {oracle.get('mode')!r}")
    frozen = _load_oracle(oracle, FIXTURES)
    stdout = frozen.get("stdout")
    files = frozen.get("files", {})
    if not isinstance(stdout, str) or not isinstance(files, dict) or not all(
        isinstance(path, str) and isinstance(contents, str) for path, contents in files.items()
    ):
        raise SequenceError("mimi_stdout oracle must contain string stdout and string file outputs")
    return stdout, files


def run_entry(
    entry: dict[str, Any],
    *,
    mimi: Path,
    ffi: Path | None,
    timeout: float,
    directory: Path,
) -> dict[str, Any]:
    case_id = entry["id"]
    source_value = entry.get("source")
    if not isinstance(source_value, str) or not source_value:
        return {
            "id": case_id,
            "source": None,
            "source_sha256": None,
            "check": "blocked",
            "vm": "not_run",
            "native": "not_run",
            "diagnostics": "fixture has no materialized source entrypoint",
        }
    try:
        source = confined_file(FIXTURES, source_value, field=f"{case_id} source")
    except SequenceError as error:
        return {
            "id": case_id,
            "source": source_value,
            "source_sha256": None,
            "check": "blocked",
            "vm": "not_run",
            "native": "not_run",
            "diagnostics": str(error),
        }
    try:
        expected, expected_files = expected_for_entry(entry["oracle"])
    except (SequenceError, KeyError) as error:
        return {
            "id": case_id,
            "source": str(source.relative_to(ROOT)) if source.is_relative_to(ROOT) else str(source),
            "source_sha256": digest(source) if source.is_file() else None,
            "check": "unsupported",
            "vm": "not_run",
            "native": "not_run",
            "diagnostics": str(error),
        }
    env = os.environ.copy()
    env.setdefault("LLVM_SYS_181_PREFIX", "/tmp/llvm-wrapper")
    if ffi is not None:
        env["MIMI_FFI_LIB"] = str(ffi)

    check = run_command([str(mimi), "check", str(source)], cwd=directory, env=env, timeout=timeout)
    record: dict[str, Any] = {
        "id": case_id,
        "source": str(source.relative_to(ROOT)),
        "source_sha256": digest(source),
        "expected": expected,
        "check": "pass" if check["returncode"] == 0 else "fail",
        "vm": "not_run",
        "native": "not_run",
        "diagnostics": (check["stderr"] or check["stdout"])[-3000:],
    }
    if check["returncode"] != 0:
        record["vm"] = "not_run"
        record["native"] = "not_run"
        return record

    vm = run_command([str(mimi), "run", str(source)], cwd=directory, env=env, timeout=timeout)
    vm_out = vm["stdout"]
    if vm["timed_out"]:
        record["vm"] = "timeout"
    elif vm["returncode"] != 0:
        record["vm"] = "failed"
    elif vm_out != expected:
        record["vm"] = "mismatch"
    else:
        record["vm"] = "pass"
        for path, contents in expected_files.items():
            output = directory / path
            if not output.exists() or output.read_text(encoding="utf-8") != contents:
                record["vm"] = "file_mismatch"
                break
    record["vm_stdout"] = vm_out
    record["vm_stderr"] = vm["stderr"][-2000:]

    executable = directory / f"mimi-v02-{case_id.lower()}"
    build_command = [str(mimi), "build", str(source), "-o", str(executable)]
    if ffi is not None:
        build_command.extend(["-L", str(ffi.parent), "-l", "v02ffi"])
    build = run_command(build_command, cwd=directory, env=env, timeout=max(timeout, 120.0))
    build_text = f"{build['stdout']}\n{build['stderr']}"
    if build["timed_out"]:
        record["native"] = "timeout"
    elif build["returncode"] != 0 or not executable.exists():
        record["native"] = "unsupported" if unsupported_diagnostic(build_text) else "build_failed"
    else:
        native_env = env.copy()
        if ffi is not None:
            native_env["LD_LIBRARY_PATH"] = f"{ffi.parent}:{native_env.get('LD_LIBRARY_PATH', '')}"
        native = run_command([str(executable)], cwd=directory, env=native_env, timeout=timeout)
        if native["timed_out"]:
            record["native"] = "timeout"
        elif native["returncode"] != 0:
            record["native"] = "run_failed"
        elif native["stdout"] != expected:
            record["native"] = "mismatch"
        else:
            record["native"] = "pass"
        record["native_stdout"] = native["stdout"]
        record["native_stderr"] = native["stderr"][-2000:]
    record["native_diagnostics"] = build_text[-3000:]
    if executable.exists():
        record["native_sha256"] = digest(executable)
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mimi", type=Path, default=Path(os.environ.get("MIMI", str(ROOT / "target" / "debug" / "mimi"))))
    parser.add_argument("--case", action="append", dest="cases", help="run only this case id (repeatable)")
    parser.add_argument("--sequence", action="store_true", help="apply and evaluate the declared SEQ-W2 and SEQ-W4 cumulative patch chains")
    parser.add_argument("--sequence-dir", type=Path, help="new, retained directory for cumulative source snapshots and sequence evidence")
    parser.add_argument("--controls", action="store_true", help="run the ASan/TSan detector controls")
    parser.add_argument("--strict", action="store_true", help="fail if a native case is unsupported or a detector is unavailable")
    parser.add_argument("--out", type=Path, help="write machine-readable JSON result")
    parser.add_argument("--timeout", type=float, default=45.0)
    parser.add_argument("--cc", default=os.environ.get("CC", "cc"))
    args = parser.parse_args()
    if args.sequence and not args.sequence_dir and not args.out:
        parser.error("--sequence requires --sequence-dir or --out so its cumulative artifacts can be retained")

    # ``run_entry`` executes from a retained temporary work directory so that
    # fixture outputs and native artifacts cannot pollute the checkout.  Make
    # the compiler path absolute before entering that directory; otherwise a
    # valid invocation such as ``--mimi target/debug/mimi`` is looked up
    # relative to the temporary directory and every fixture is misclassified
    # as a check failure.
    mimi = args.mimi.expanduser().resolve()
    if not mimi.exists():
        print(f"mimi executable not found: {mimi}", file=sys.stderr)
        return 2
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    frozen = json.loads(EXPECTED_PATH.read_text(encoding="utf-8"))
    oracle_errors = check_expected_json(frozen)
    if oracle_errors:
        for error in oracle_errors:
            print(f"ORACLE ERROR: {error}", file=sys.stderr)
        return 2

    filters = set(args.cases or ())
    entries = selected_entries(manifest, filters or None)
    output: dict[str, Any] = {
        "schema": "mimi-p6-app-results-v2",
        "mimi": str(mimi),
        "mimi_sha256": digest(mimi),
        "entries": [],
    }
    hard_failure = False
    with tempfile.TemporaryDirectory(prefix="mimi-v02-app-") as temporary:
        work = Path(temporary)
        ffi, ffi_error = compile_ffi(args.cc, work)
        if ffi_error:
            output["ffi"] = {"status": "environment_unverified", "detail": ffi_error}
        else:
            output["ffi"] = {"status": "built", "sha256": digest(ffi)}
        for entry in entries:
            record = run_entry(entry, mimi=mimi, ffi=ffi, timeout=args.timeout, directory=work)
            output["entries"].append(record)
            print(f"{record['id']:<16} check={record['check']:<4} vm={record['vm']:<14} native={record['native']}")
            if record["check"] != "pass" or record["vm"] != "pass":
                hard_failure = True

        if args.sequence:
            if args.sequence_dir:
                artifact_root = args.sequence_dir.expanduser().resolve()
            else:
                assert args.out is not None
                artifact_root = args.out.expanduser().resolve().with_name(args.out.stem + ".sequence-artifacts")
            try:
                artifact_root.mkdir(parents=True, exist_ok=False)
            except FileExistsError:
                print(f"sequence artifact directory already exists: {artifact_root}", file=sys.stderr)
                return 2
            output["sequence_artifacts"] = str(artifact_root)
            sequences: dict[str, Any] = {}
            declared_sequences = manifest.get("sequences")
            if not isinstance(declared_sequences, list):
                for sequence_id in EXPECTED_SEQUENCES:
                    sequences[sequence_id] = {
                        "id": sequence_id,
                        "status": "failed",
                        "accepted": False,
                        "detail": "manifest does not declare the required v2 cumulative sequence objects",
                    }
                    hard_failure = True
            else:
                declared_ids = [item.get("id") if isinstance(item, dict) else None for item in declared_sequences]
                if declared_ids != list(EXPECTED_SEQUENCES):
                    for sequence_id in EXPECTED_SEQUENCES:
                        sequences[sequence_id] = {
                            "id": sequence_id,
                            "status": "failed",
                            "accepted": False,
                            "detail": "manifest sequence inventory is missing, extra, or reordered",
                        }
                        hard_failure = True
                else:
                    for sequence in declared_sequences:
                        sequence_id = sequence.get("id")
                        try:
                            result = run_sequence(
                                sequence,
                                mimi=mimi,
                                timeout=args.timeout,
                                fixtures_root=FIXTURES,
                                artifact_root=artifact_root,
                                cc=args.cc,
                            )
                        except SequenceError as error:
                            result = {
                                "id": sequence_id,
                                "execution": sequence.get("execution"),
                                "status": "failed",
                                "accepted": False,
                                "detail": str(error),
                                "blockers": sequence.get("blockers", []),
                                "checkpoints": [],
                            }
                        sequences[sequence_id] = result
                        print(f"{sequence_id:<16} sequence={result['status']}")
                        if result.get("status") != "pass":
                            hard_failure = True
            output["sequences"] = sequences
            (artifact_root / "run.json").write_text(
                json.dumps({"schema": output["schema"], "mimi": output["mimi"], "mimi_sha256": output["mimi_sha256"], "sequences": sequences}, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )

        if args.controls:
            output["detectors"] = detector_case(args.cc, work)

    if args.controls:
        for sanitizer, result in output.get("detectors", {}).items():
            print(f"detector {sanitizer}: {result['status']}")
            if result["status"] != "detected":
                hard_failure = True
    if args.strict:
        for row in output["entries"]:
            if row["native"] != "pass":
                hard_failure = True
        for sequence in output.get("sequences", {}).values():
            for checkpoint in sequence.get("checkpoints", []):
                for regression in checkpoint.get("regressions", []):
                    if regression.get("status") != "pass":
                        hard_failure = True
                    if regression.get("mode") in ("mimi_stdout", "w4_host_driver") and regression.get("native") != "pass":
                        hard_failure = True

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"cases={len(output['entries'])} hard_failure={str(hard_failure).lower()}")
    return 1 if hard_failure else 0


if __name__ == "__main__":
    raise SystemExit(main())
