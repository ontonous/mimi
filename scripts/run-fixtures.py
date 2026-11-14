#!/usr/bin/env python3
"""Run declarative Mimi 0.2 fixtures against independent expectations.

Example fixture manifest (JSON):

```
{
  "schema_version": 1,
  "fixtures": [{
    "id": "w1-json-roundtrip",
    "task_id": "B-02",
    "inputs": ["fixtures/w1/input.json"],
    "command": ["python3", "fixtures/w1/candidate.py"],
    "oracle": {"command": ["python3", "fixtures/w1/oracle.py"]},
    "expected": {"result_class": "已通过", "exit_code": 0},
    "artifacts": [{"path": "fixtures/w1/report.json", "sha256": "..."}]
  }]
}
```

The candidate and oracle are separate processes.  A fixture is accepted only
when the declared expected status and output/artifact checks match; a command
that exits zero while running no required tests is not accepted.  Every log,
record, and aggregate manifest is written with exclusive creation, so a later
run cannot silently replace earlier evidence.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
from typing import Any, Mapping


SCRIPT = Path(__file__).resolve()
SCRIPTS = SCRIPT.parent
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

import evidence_manifest as evidence  # noqa: E402  (local script module)


TEST_RESULT = re.compile(
    r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
    r"(\d+) measured; (\d+) filtered out"
)
MAX_CAPTURE_BYTES = 8 * 1024 * 1024
PASS = "已通过"
REJECT = "明确拒绝"
ENVIRONMENT = "环境未验证"
FAILURE = "未解释失败"


@dataclass
class CommandResult:
    argv: list[str]
    exit_code: int | None
    stdout: bytes
    stderr: bytes
    elapsed_seconds: float
    launch_error: str | None = None
    timed_out: bool = False
    output_truncated: bool = False


def _safe_id(value: str) -> bool:
    return bool(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", value))


def _bounded(data: bytes) -> tuple[bytes, bool]:
    if len(data) <= MAX_CAPTURE_BYTES:
        return data, False
    return data[:MAX_CAPTURE_BYTES], True


def _run(argv: list[str], *, cwd: Path, env: Mapping[str, str], timeout: float) -> CommandResult:
    started = time.monotonic()
    try:
        process = subprocess.run(
            argv,
            cwd=cwd,
            env=dict(env),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
        stdout, stdout_truncated = _bounded(process.stdout or b"")
        stderr, stderr_truncated = _bounded(process.stderr or b"")
        return CommandResult(
            argv=argv,
            exit_code=process.returncode,
            stdout=stdout,
            stderr=stderr,
            elapsed_seconds=time.monotonic() - started,
            output_truncated=stdout_truncated or stderr_truncated,
        )
    except FileNotFoundError as error:
        return CommandResult(
            argv=argv,
            exit_code=None,
            stdout=b"",
            stderr=b"",
            elapsed_seconds=time.monotonic() - started,
            launch_error=str(error),
        )
    except PermissionError as error:
        return CommandResult(
            argv=argv,
            exit_code=None,
            stdout=b"",
            stderr=b"",
            elapsed_seconds=time.monotonic() - started,
            launch_error=str(error),
        )
    except subprocess.TimeoutExpired as error:
        stdout, stdout_truncated = _bounded(error.stdout or b"")
        stderr, stderr_truncated = _bounded(error.stderr or b"")
        return CommandResult(
            argv=argv,
            exit_code=None,
            stdout=stdout,
            stderr=stderr,
            elapsed_seconds=time.monotonic() - started,
            launch_error=f"command timed out after {timeout:g}s",
            timed_out=True,
            output_truncated=stdout_truncated or stderr_truncated,
        )


def _text(data: bytes) -> str:
    return data.decode("utf-8", errors="replace")


def _write_new(path: Path, data: bytes) -> None:
    """Write one evidence file with an exclusive create operation."""

    with path.open("xb") as stream:
        stream.write(data)


def _test_counts(result: CommandResult) -> dict[str, Any]:
    output = _text(result.stdout) + "\n" + _text(result.stderr)
    summaries = []
    for row in TEST_RESULT.findall(output):
        summaries.append(
            {
                "status": row[0],
                "passed": int(row[1]),
                "failed": int(row[2]),
                "ignored": int(row[3]),
                "measured": int(row[4]),
                "filtered_out": int(row[5]),
            }
        )
    totals = {
        key: sum(item[key] for item in summaries)
        for key in ("passed", "failed", "ignored", "measured", "filtered_out")
    }
    return {
        "summaries": summaries,
        "executed": totals["passed"] + totals["failed"],
        "discovered": sum(totals.values()),
        **totals,
    }


def _executable_record(argv: list[str], *, cwd: Path, env: Mapping[str, str], role: str) -> dict[str, Any]:
    if not argv:
        return {"role": role, "path": None, "status": "missing", "sha256": None}
    command = Path(argv[0])
    if command.is_absolute() or command.parent != Path("."):
        resolved = (command if command.is_absolute() else cwd / command).resolve()
    else:
        found = shutil.which(argv[0], path=env.get("PATH"))
        resolved = Path(found).resolve() if found else None
    if resolved is None or not resolved.is_file():
        return {"role": role, "path": str(resolved) if resolved else argv[0], "status": "missing", "sha256": None}
    return {
        "role": role,
        "path": str(resolved),
        "status": "observed",
        "sha256": evidence.sha256_file(resolved),
        "bytes": resolved.stat().st_size,
    }


def _command_source_files(root: Path, cwd: Path, argv: list[str]) -> set[Path]:
    """Find existing repository files named directly in a command argv."""

    found: set[Path] = set()
    for value in argv[1:]:
        path = Path(value)
        candidate = path if path.is_absolute() else cwd / path
        try:
            resolved = candidate.resolve()
            resolved.relative_to(root.resolve())
        except (OSError, ValueError):
            continue
        if resolved.is_file():
            found.add(resolved)
    return found


def _inside(root: Path, value: str) -> Path:
    path = Path(value)
    if not path.is_absolute():
        path = root / path
    path = path.resolve()
    # Fixture inputs/outputs should not escape the selected repository root.
    # Commands may still write /tmp or another external scratch directory.
    try:
        path.relative_to(root.resolve())
    except ValueError as error:
        raise ValueError(f"fixture path escapes repository root: {value}") from error
    return path


def _artifact_path(root: Path, value: str) -> Path:
    """Resolve an explicitly named output, allowing host/FFI paths outside root."""

    path = Path(value)
    if not path.is_absolute():
        path = root / path
    return path.resolve()


def _expected_spec(fixture: Mapping[str, Any]) -> tuple[str, dict[str, Any]]:
    expected = dict(fixture.get("expected", {}))
    label = evidence.normalize_result_class(
        expected.get("result_class", expected.get("status", PASS)), default=PASS
    )
    expected["result_class"] = label
    return label, expected


def _compare_stream(
    actual: bytes,
    expected: Mapping[str, Any],
    name: str,
) -> list[str]:
    errors: list[str] = []
    text = _text(actual)
    if name in expected and expected[name] is not None:
        wanted = expected[name]
        if not isinstance(wanted, str):
            errors.append(f"expected {name} must be text")
        elif text != wanted:
            errors.append(f"{name} differs from expected text")
    digest_name = f"{name}_sha256"
    if digest_name in expected and expected[digest_name] is not None:
        wanted = expected[digest_name]
        if wanted != evidence.sha256_bytes(actual):
            errors.append(f"{name} hash differs from expected")
    regex_name = f"{name}_regex"
    if regex_name in expected and expected[regex_name] is not None:
        try:
            matched = re.search(str(expected[regex_name]), text, re.MULTILINE) is not None
        except re.error as error:
            errors.append(f"invalid {regex_name}: {error}")
        else:
            if not matched:
                errors.append(f"{name} does not match expected regex")
    return errors


def _oracle_expectation(
    fixture: Mapping[str, Any],
    *,
    root: Path,
    env: Mapping[str, str],
    timeout: float,
) -> tuple[dict[str, Any], CommandResult | None]:
    oracle = fixture.get("oracle")
    if oracle is None:
        return {}, None
    if isinstance(oracle, dict) and "command" in oracle:
        argv = list(oracle["command"])
        cwd = _inside(root, oracle.get("cwd", "."))
        oracle_env = dict(env)
        oracle_env.update({str(k): str(v) for k, v in oracle.get("env", {}).items()})
        result = _run(argv, cwd=cwd, env=oracle_env, timeout=float(oracle.get("timeout_seconds", timeout)))
        return {
            "exit_code": result.exit_code,
            "stdout": _text(result.stdout),
            "stderr": _text(result.stderr),
            "stdout_sha256": evidence.sha256_bytes(result.stdout),
            "stderr_sha256": evidence.sha256_bytes(result.stderr),
            "launch_error": result.launch_error,
        }, result
    if not isinstance(oracle, dict):
        raise ValueError("oracle must be an object with command or expected stream fields")
    return dict(oracle), None


def _artifact_expectations(
    root: Path,
    specs: Any,
) -> tuple[list[dict[str, Any]], list[str]]:
    if not isinstance(specs, list):
        raise ValueError("fixture artifacts must be an array")
    records: list[dict[str, Any]] = []
    errors: list[str] = []
    for value in specs:
        if isinstance(value, str):
            path_value = value
            expected_hash = None
            required = True
            kind = None
        elif isinstance(value, dict):
            path_value = value.get("path")
            expected_hash = value.get("sha256")
            required = bool(value.get("required", True))
            kind = value.get("kind")
        else:
            raise ValueError("artifact entry must be a path or object")
        if not isinstance(path_value, str) or not path_value:
            raise ValueError("artifact path must be text")
        # Artifact paths are explicit output declarations.  They may point at
        # a host-C/runtime build directory outside the repository; source and
        # fixture inputs remain constrained by ``_inside`` above.
        path = _artifact_path(root, path_value)
        record = evidence.artifact_record(path, root=root, kind=kind, required=required)
        records.append(record)
        if expected_hash is not None and record.get("sha256") != expected_hash:
            errors.append(f"artifact hash differs: {path_value}")
        if expected_hash is None and required:
            errors.append(f"artifact has no expected sha256: {path_value}")
        if required and record.get("status") != "observed":
            errors.append(f"required artifact is missing: {path_value}")
    return records, errors


def _exception_record(fixture: Mapping[str, Any], error: BaseException) -> dict[str, Any]:
    """Keep fixture setup/runner exceptions visible in the aggregate record set."""

    fixture_id = str(fixture.get("id", "unknown"))
    record: dict[str, Any] = {
        "schema_version": evidence.SCHEMA_VERSION,
        "evidence_id": fixture_id,
        "fixture_id": fixture_id,
        "task_id": fixture.get("task_id"),
        "execution": "not_started_or_incomplete",
        "checks_passed": False,
        "accepted": False,
        "provisional": True,
        "result_class": FAILURE,
        "errors": [f"fixture collection raised {type(error).__name__}: {error}"],
    }
    record["record_hash"] = evidence.digest_json(record)
    return record


def run_fixture(
    fixture: Mapping[str, Any],
    *,
    root: Path,
    fixture_manifest_path: Path,
    evidence_dir: Path,
    plan_path: Path | None,
    default_timeout: float,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    evidence_dir.mkdir(parents=True, exist_ok=True)
    fixture_id = str(fixture["id"])
    if not _safe_id(fixture_id):
        raise ValueError(f"invalid fixture id: {fixture_id}")
    task_id = fixture.get("task_id")
    if task_id is not None and (not isinstance(task_id, str) or not re.fullmatch(r"[A-Z]-\d{2}", task_id)):
        raise ValueError(f"fixture {fixture_id}: invalid task_id")
    expected_label, expected = _expected_spec(fixture)
    plan_before = evidence.plan_record(plan_path)
    inputs = [fixture_manifest_path]
    input_paths: list[Path] = []
    for value in fixture.get("inputs", []):
        path = _inside(root, value)
        input_paths.append(path)
        inputs.append(path)
    before = evidence.source_snapshot(root, inputs)
    missing_inputs = [
        name for name, item in before["inputs"].items() if item.get("missing")
    ]
    env = dict(os.environ)
    env.update({str(key): str(value) for key, value in fixture.get("env", {}).items()})
    command = fixture.get("command", fixture.get("candidate"))
    if isinstance(command, dict):
        command = command.get("command")
    if not isinstance(command, list):
        raise ValueError(f"fixture {fixture_id}: command must be an argv list")
    cwd = _inside(root, fixture.get("cwd", "."))
    declared_inputs = {path.resolve() for path in input_paths}
    command_files = _command_source_files(root, cwd, command)
    if isinstance(fixture.get("oracle"), dict) and isinstance(fixture["oracle"].get("command"), list):
        oracle_cwd = _inside(root, fixture["oracle"].get("cwd", "."))
        command_files.update(_command_source_files(root, oracle_cwd, fixture["oracle"]["command"]))
    untracked_command_files = sorted(str(path) for path in command_files - declared_inputs)
    timeout = float(fixture.get("timeout_seconds", default_timeout))
    candidate = _run(command, cwd=cwd, env=env, timeout=timeout)
    oracle, oracle_result = _oracle_expectation(fixture, root=root, env=env, timeout=timeout)
    errors: list[str] = []
    if untracked_command_files:
        errors.append("command source files must be declared as inputs: " + ", ".join(untracked_command_files))
    if missing_inputs:
        errors.append("declared input is missing: " + ", ".join(sorted(missing_inputs)))
    if candidate.timed_out:
        actual_label = FAILURE
    elif candidate.launch_error:
        actual_label = ENVIRONMENT
    else:
        actual_label = expected_label if expected_label == REJECT and candidate.exit_code != 0 else (
            PASS if candidate.exit_code == 0 else FAILURE
        )
    # A missing oracle is fine when the fixture declares literal output/exit
    # expectations.  A passing fixture with neither is rejected at comparison
    # time instead of becoming an accidental skip.
    if oracle:
        if "exit_code" in oracle and candidate.exit_code != oracle.get("exit_code"):
            errors.append("exit code differs from oracle")
        errors.extend(_compare_stream(candidate.stdout, oracle, "stdout"))
        errors.extend(_compare_stream(candidate.stderr, oracle, "stderr"))
        if oracle_result and oracle_result.launch_error:
            errors.append("oracle could not be executed")
    elif any(key in expected for key in ("exit_code", "stdout", "stderr", "stdout_sha256", "stderr_sha256", "stdout_regex", "stderr_regex")):
        if "exit_code" in expected and candidate.exit_code != expected.get("exit_code"):
            errors.append("exit code differs from expected")
        errors.extend(_compare_stream(candidate.stdout, expected, "stdout"))
        errors.extend(_compare_stream(candidate.stderr, expected, "stderr"))
    elif expected_label == PASS:
        errors.append("passing fixture needs an oracle or explicit expectation")
    if expected_label == PASS and candidate.exit_code != 0:
        errors.append("passing fixture did not exit successfully")
    stream_keys = ("stdout", "stderr", "stdout_sha256", "stderr_sha256", "stdout_regex", "stderr_regex")
    has_stream_expectation = any(expected.get(key) is not None for key in stream_keys)
    if oracle:
        has_stream_expectation = has_stream_expectation or any(oracle.get(key) is not None for key in stream_keys)
    artifact_specs = fixture.get("artifacts", [])
    has_hashed_artifact = isinstance(artifact_specs, list) and any(
        isinstance(item, dict) and item.get("required", True) and isinstance(item.get("sha256"), str)
        for item in artifact_specs
    )
    if expected_label == PASS and not (has_stream_expectation or has_hashed_artifact):
        errors.append("passing fixture needs a behavioral oracle, stream expectation, or required hashed artifact; exit code alone is insufficient")
    if expected_label == REJECT and candidate.exit_code == 0:
        errors.append("reject fixture exited successfully")
    if expected_label == REJECT:
        has_diagnostic = has_stream_expectation or bool(oracle_result)
        if "exit_code" not in expected and not (oracle and "exit_code" in oracle):
            errors.append("reject fixture must declare the expected nonzero exit code")
        if not has_diagnostic:
            errors.append("reject fixture needs an independent oracle or diagnostic expectation")
    if fixture.get("require_tests", False):
        counts = _test_counts(candidate)
        command_name = Path(command[0]).name.lower() if command else ""
        is_test_runner = command_name in {"cargo", "mimi"} and "test" in command[1:] and "--no-run" not in command
        if not is_test_runner:
            errors.append("required test fixture must invoke cargo test or mimi test directly")
        if counts["executed"] <= 0 or counts["failed"] != 0 or not counts["summaries"]:
            errors.append("required test fixture ran zero tests or reported failures")
        if any(item["status"].lower() != "ok" for item in counts["summaries"]):
            errors.append("required test fixture reported a non-ok test summary")
    else:
        counts = _test_counts(candidate)
    after = evidence.source_snapshot(root, inputs)
    plan_after = evidence.plan_record(plan_path)
    source_stable = before["source_identity"] == after["source_identity"]
    if not source_stable:
        errors.append("fixture inputs or source identity changed during execution")
    if plan_before != plan_after:
        errors.append("plan identity changed during execution")
    artifact_records, artifact_errors = _artifact_expectations(root, fixture.get("artifacts", []))
    errors.extend(artifact_errors)
    if candidate.output_truncated:
        errors.append("captured output exceeded 8 MiB")
    accepted = not errors and (
        (expected_label == PASS and candidate.exit_code == 0)
        or (expected_label == REJECT and candidate.exit_code not in (None, 0))
    )
    if expected_label == REJECT and accepted:
        actual_label = REJECT
    elif accepted:
        actual_label = PASS
    elif candidate.timed_out:
        actual_label = FAILURE
    elif candidate.launch_error:
        actual_label = ENVIRONMENT
    elif expected_label == REJECT and candidate.exit_code not in (None, 0) and not errors:
        actual_label = REJECT
    elif expected_label in {"不支持", ENVIRONMENT, "未运行"}:
        actual_label = expected_label
    else:
        actual_label = FAILURE
    log_stem = evidence_dir / fixture_id
    # Append suffixes instead of Path.with_suffix: fixture IDs may contain a
    # dot, and ``foo.bar`` must not collide with ``foo`` evidence.
    stdout_path = Path(str(log_stem) + ".stdout")
    stderr_path = Path(str(log_stem) + ".stderr")
    oracle_stdout_path = Path(str(log_stem) + ".oracle.stdout") if oracle_result else None
    oracle_stderr_path = Path(str(log_stem) + ".oracle.stderr") if oracle_result else None
    record_path = Path(str(log_stem) + ".json")
    for path in (stdout_path, stderr_path, oracle_stdout_path, oracle_stderr_path, record_path):
        if path is None:
            continue
        if path.exists():
            raise RuntimeError(f"preserve existing fixture evidence: {path}")
    _write_new(stdout_path, candidate.stdout)
    _write_new(stderr_path, candidate.stderr)
    if oracle_result and oracle_stdout_path and oracle_stderr_path:
        _write_new(oracle_stdout_path, oracle_result.stdout)
        _write_new(oracle_stderr_path, oracle_result.stderr)
    requirement = fixture.get("requirement", "")
    requirement_hash = evidence.digest_json({"requirement": requirement, "expected": expected})
    record: dict[str, Any] = {
        "schema_version": evidence.SCHEMA_VERSION,
        "evidence_id": fixture_id,
        "fixture_id": fixture_id,
        "task_id": task_id,
        "requirement": requirement,
        "requirement_hash": requirement_hash,
        "plan_hash": plan_before.get("sha256"),
        "plan_identity_before": plan_before,
        "plan_identity_after": plan_after,
        "decision_revision": fixture.get("decision_revision"),
        "command": command,
        "cwd": str(cwd),
        "environment": evidence.environment_snapshot(fixture.get("record_environment", []), environment=env),
        "expected": expected,
        "expected_result_class": expected_label,
        "sequence_id": fixture.get("sequence_id"),
        "checkpoint": fixture.get("checkpoint"),
        "parent_hash": fixture.get("parent_hash"),
        "source_patch": fixture.get("source_patch"),
        "seed": fixture.get("seed"),
        "parameters": fixture.get("parameters", {}),
        "actual": {
            "exit_code": candidate.exit_code,
            "stdout": _text(candidate.stdout),
            "stderr": _text(candidate.stderr),
            "stdout_sha256": evidence.sha256_bytes(candidate.stdout),
            "stderr_sha256": evidence.sha256_bytes(candidate.stderr),
            "launch_error": candidate.launch_error,
            "timed_out": candidate.timed_out,
        },
        "oracle": oracle,
        "input_files": before["inputs"],
        "source_identity_before": before["source_identity"],
        "source_identity_after": after["source_identity"],
        "source_stable": source_stable,
        "artifacts": artifact_records,
        "command_executables": [
            _executable_record(command, cwd=cwd, env=env, role="candidate"),
            *(
                [_executable_record(list(fixture["oracle"]["command"]), cwd=_inside(root, fixture["oracle"].get("cwd", ".")), env={**env, **{str(k): str(v) for k, v in fixture["oracle"].get("env", {}).items()}}, role="oracle")]
                if isinstance(fixture.get("oracle"), dict) and isinstance(fixture["oracle"].get("command"), list)
                else []
            ),
        ],
        "test_counts": counts,
        "started_at_unix": time.time() - candidate.elapsed_seconds,
        "elapsed_seconds": candidate.elapsed_seconds,
        "raw_log_paths": {
            "stdout": str(stdout_path),
            "stderr": str(stderr_path),
        },
        "raw_log_hashes": {
            "stdout": evidence.sha256_file(stdout_path),
            "stderr": evidence.sha256_file(stderr_path),
        },
        "errors": errors,
        "checks_passed": accepted,
        "accepted": False,
        "provisional": True,
        "acceptance_reason": "no verified external runner attestation is available",
        "result_class": actual_label,
        "limitations": [
            "The oracle is independent only to the extent declared by the fixture manifest.",
            "Boundary source snapshots cannot observe edits that are reverted before the command exits.",
        ],
    }
    if oracle_result and oracle_stdout_path and oracle_stderr_path:
        record["oracle_log_paths"] = {
            "stdout": str(oracle_stdout_path),
            "stderr": str(oracle_stderr_path),
        }
        record["oracle_log_hashes"] = {
            "stdout": evidence.sha256_file(oracle_stdout_path),
            "stderr": evidence.sha256_file(oracle_stderr_path),
        }
    record["record_hash"] = evidence.digest_json(record)
    _write_new(
        record_path,
        (json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8"),
    )
    artifacts = artifact_records + [
        evidence.artifact_record(stdout_path, root=evidence_dir, kind="raw-log"),
        evidence.artifact_record(stderr_path, root=evidence_dir, kind="raw-log"),
        evidence.artifact_record(record_path, root=evidence_dir, kind="fixture-record"),
    ]
    artifacts.extend(
        evidence.artifact_record(item["path"], root=root, kind=f"{item['role']}-executable")
        for item in record["command_executables"]
        if item.get("status") == "observed" and item.get("path")
    )
    if oracle_result and oracle_stdout_path and oracle_stderr_path:
        artifacts.extend(
            [
                evidence.artifact_record(oracle_stdout_path, root=evidence_dir, kind="oracle-log"),
                evidence.artifact_record(oracle_stderr_path, root=evidence_dir, kind="oracle-log"),
            ]
        )
    return record, artifacts


def run_all(args: argparse.Namespace) -> int:
    root = Path(args.root).resolve()
    manifest_path = Path(args.manifest).resolve()
    evidence_dir = Path(args.evidence).resolve()
    evidence_dir.mkdir(parents=True, exist_ok=True)
    fixture_data = evidence.load_fixture_manifest(manifest_path)
    expected_fixture_ids = [item["id"] for item in fixture_data["fixtures"]]
    selected = fixture_data["fixtures"]
    if args.fixture:
        wanted = set(args.fixture)
        selected = [item for item in selected if item["id"] in wanted]
        missing = wanted - {item["id"] for item in selected}
        if missing:
            raise ValueError(f"unknown fixture: {', '.join(sorted(missing))}")
    if not selected:
        raise ValueError("no fixtures selected; zero fixtures cannot be accepted")
    if args.plan:
        plan_path = Path(args.plan).resolve()
        if not plan_path.is_file():
            raise ValueError(f"declared plan is missing: {plan_path}")
    else:
        default_plan = root / "devdocs" / "V0.2" / "plan.toml"
        plan_path = default_plan if default_plan.is_file() else None
    known_task_ids = evidence.plan_task_ids(plan_path)
    if known_task_ids:
        unknown = sorted({item["task_id"] for item in selected if item.get("task_id") and item["task_id"] not in known_task_ids})
        if unknown:
            raise ValueError("fixture references unknown plan task: " + ", ".join(unknown))
    aggregate_plan_before = evidence.plan_record(plan_path)
    source_paths: list[Path] = [manifest_path]
    invalid_fixture_inputs: dict[str, ValueError] = {}
    for fixture in selected:
        for value in fixture.get("inputs", []):
            try:
                source_paths.append(_inside(root, value))
            except ValueError as error:
                invalid_fixture_inputs[str(fixture["id"])] = error
    records: list[dict[str, Any]] = []
    artifacts: list[dict[str, Any]] = [evidence.artifact_record(manifest_path, root=root, kind="fixture-manifest")]
    artifacts.append(evidence.artifact_record(SCRIPT, root=root, kind="runner"))
    if plan_path and plan_path.is_file():
        artifacts.append(evidence.artifact_record(plan_path, root=root, kind="plan"))
    errors: list[str] = []
    selected_ids = [item["id"] for item in selected]
    if selected_ids != expected_fixture_ids:
        errors.append("fixture selection is incomplete; partial runs are provisional and cannot satisfy the release inventory")
    for fixture in selected:
        fixture_id = str(fixture.get("id", "unknown"))
        if fixture_id in invalid_fixture_inputs:
            error = invalid_fixture_inputs[fixture_id]
            errors.append(f"{fixture_id}: {error}")
            records.append(_exception_record(fixture, error))
            continue
        try:
            record, fixture_artifacts = run_fixture(
                fixture,
                root=root,
                fixture_manifest_path=manifest_path,
                evidence_dir=evidence_dir,
                plan_path=plan_path,
                default_timeout=args.timeout,
            )
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            errors.append(f"{fixture_id}: {error}")
            records.append(_exception_record(fixture, error))
            continue
        records.append(record)
        artifacts.extend(fixture_artifacts)
    aggregate_plan_after = evidence.plan_record(plan_path)
    if aggregate_plan_before != aggregate_plan_after:
        errors.append("plan identity changed during fixture run")
    manifest = evidence.create_manifest(
        root=root,
        evidence_dir=evidence_dir,
        plan_path=plan_path,
        source_paths=source_paths,
        records=records,
        artifacts=artifacts,
        task_ids=[item["task_id"] for item in selected if item.get("task_id")],
        expected_fixture_ids=expected_fixture_ids,
        errors=errors,
        fixture_manifest={
            "path": str(manifest_path),
            "sha256": evidence.sha256_file(manifest_path),
            "fixture_count": len(expected_fixture_ids),
            "selected_count": len(selected),
            "selected_ids": selected_ids,
        },
        toolchain={
            "status": "not-collected",
            "reason": "fixture runner records command environment; use release_evidence.py for compiler/toolchain probes",
        },
        metadata={
            "runner": str(SCRIPT),
            "runner_sha256": evidence.sha256_file(SCRIPT),
            "errors": errors,
            "plan_identity_before": aggregate_plan_before,
            "plan_identity_after": aggregate_plan_after,
        },
    )
    output = Path(args.output).resolve() if args.output else evidence_dir / "manifest.json"
    evidence.write_manifest(output, manifest)
    provisional_accepted = bool(manifest["provisional_accepted"])
    accepted = False
    print(
        json.dumps(
            {
                "manifest": str(output),
                "fixtures": len(selected),
                "records": len(records),
                "accepted": accepted,
                "provisional_accepted": provisional_accepted,
                "result_counts": manifest["result_counts"],
                "errors": errors,
            },
            ensure_ascii=False,
        )
    )
    return 0 if provisional_accepted else 2


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, help="JSON fixture manifest")
    parser.add_argument("--evidence", required=True, help="new evidence directory")
    parser.add_argument("--output", help="immutable aggregate manifest path (default: evidence/manifest.json)")
    parser.add_argument("--root", default=str(Path(__file__).resolve().parents[1]), help="repository root")
    parser.add_argument("--plan", help="plan.toml to bind into records")
    parser.add_argument("--fixture", action="append", help="run only this fixture id (repeatable)")
    parser.add_argument("--timeout", type=float, default=120.0, help="per-command timeout in seconds")
    args = parser.parse_args()
    try:
        return run_all(args)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"fixture evidence rejected: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
