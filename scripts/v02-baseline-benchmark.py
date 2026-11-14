#!/usr/bin/env python3
"""Collect reproducible 0.2 baseline and benchmark evidence.

This runner is deliberately independent of Cargo's test harness.  It records a
source/toolchain identity first, then (when requested) executes a fixed command
matrix for each explicitly selected Mimi input.  Every command has a route,
backend and verifier *intent* and the runner records the observed values when a
receipt or diagnostic exposes them.  Missing probes are written as ``unknown``;
they are never treated as zero or as a passing result.

The collector is useful for B-01, B-04 and B-08, but is not a release gate by
itself.  It does not change ``plan.toml``, source files or version metadata.  A
new evidence directory is required for every collection.  Existing records are
never overwritten, including records for failed or timed-out commands.

Examples (the second command is the one a maintainer normally runs)::

    python3 scripts/v02-baseline-benchmark.py snapshot \
        --evidence-dir /tmp/mimi-v02-baseline-snapshot \
        --source benchmarks/dsp.mimi

    python3 scripts/v02-baseline-benchmark.py collect \
        --evidence-dir /tmp/mimi-v02-baseline-dsp \
        --mimi target/debug/mimi --source benchmarks/dsp.mimi \
        --repeat 3 --warmup 1

For a larger, fixed matrix pass a JSON fixture manifest with ``--fixture``.
The manifest is hashed into the evidence identity.  Its command argv may use
``{mimi}``, ``{source}``, ``{artifact}``, ``{root}`` and ``{evidence}``
placeholders.  An example shape is documented by ``fixture_schema()`` and can
be emitted with ``schema``.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import resource
import shutil
import signal
import statistics
import subprocess
import sys
import time
from typing import Any, Iterable, Mapping, Sequence


SCRIPT = Path(__file__).resolve()
ROOT = SCRIPT.parent.parent
SCHEMA_VERSION = 1
EVIDENCE_KIND = "mimi-v02-baseline-benchmark"
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
    "environment": "环境未验证",
    "unverified": "环境未验证",
    "unsupported": "不支持",
    "fail": "未解释失败",
    "failed": "未解释失败",
    "not-run": "未运行",
    "not_run": "未运行",
}
IDENTIFIER = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*\Z")
PLACEHOLDER = re.compile(r"\{(mimi|source|artifact|root|evidence)\}")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def digest_json(value: Any) -> str:
    return sha256_bytes(
        json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    )


def normalize_result_class(value: Any, default: str = "未解释失败") -> str:
    if value is None:
        value = default
    if not isinstance(value, str):
        raise ValueError("result class must be text")
    result = RESULT_ALIASES.get(value.strip().lower(), value.strip())
    if result not in RESULT_CLASSES:
        raise ValueError(f"unknown result class: {value}")
    return result


def json_write_exclusive(path: Path, value: Any) -> None:
    """Write JSON once; refusing to overwrite protects failed evidence."""

    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True).encode() + b"\n"
    with path.open("xb") as stream:
        stream.write(payload)


def text_write_exclusive(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as stream:
        stream.write(data)


def git(root: Path, *args: str) -> tuple[int, bytes, bytes]:
    try:
        result = subprocess.run(
            ["git", *args], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False
        )
        return result.returncode, result.stdout, result.stderr
    except OSError as error:
        return 127, b"", str(error).encode("utf-8", errors="replace")


def git_text(root: Path, *args: str) -> str | None:
    code, stdout, _ = git(root, *args)
    if code:
        return None
    return stdout.decode("utf-8", errors="replace").strip()


def relative(path: Path, root: Path) -> str:
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return str(path.resolve())


def file_record(path: Path, root: Path) -> dict[str, Any]:
    record: dict[str, Any] = {"path": relative(path, root)}
    try:
        if not path.is_file():
            record.update({"status": "missing", "sha256": None, "bytes": None})
            return record
        record.update(
            {
                "status": "observed",
                "sha256": sha256_file(path),
                "bytes": path.stat().st_size,
                "mode": path.stat().st_mode & 0o777,
            }
        )
    except OSError as error:
        record.update({"status": "unreadable", "sha256": None, "bytes": None, "error": str(error)})
    return record


def tracked_paths(root: Path) -> list[Path]:
    code, stdout, _ = git(root, "ls-files", "-z")
    if code:
        return []
    return [root / os.fsdecode(item) for item in stdout.split(b"\0") if item]


def source_snapshot(root: Path, explicit_inputs: Iterable[str | Path]) -> dict[str, Any]:
    """Hash tracked files plus explicitly named untracked inputs.

    ``git ls-files`` gives a complete tracked census for B-01.  Explicit source
    and fixture paths are added even when they are untracked, so a newly made
    local fixture cannot silently disappear from the identity.  Target output,
    evidence directories and arbitrary scratch files are not auto-discovered.
    """

    root = root.resolve()
    paths: dict[str, Path] = {relative(path, root): path for path in tracked_paths(root)}
    for value in explicit_inputs:
        path = Path(value)
        if not path.is_absolute():
            path = root / path
        paths[relative(path, root)] = path
    inputs = {name: file_record(paths[name], root) for name in sorted(paths)}
    code, diff, diff_error = git(root, "diff", "HEAD", "--binary")
    diff_sha = sha256_bytes(diff) if code == 0 else None
    status = git_text(root, "status", "--porcelain=v1")
    source = {
        "root": str(root),
        "commit": git_text(root, "rev-parse", "HEAD"),
        "branch": git_text(root, "branch", "--show-current"),
        "tracked_diff_sha256": diff_sha,
        "git_error": None if code == 0 else diff_error.decode(errors="replace"),
        "working_tree_status": status or "",
        "inputs": inputs,
    }
    source["source_identity"] = digest_json(
        {
            "commit": source["commit"],
            "tracked_diff_sha256": source["tracked_diff_sha256"],
            "inputs": inputs,
        }
    )
    return source


def environment_snapshot() -> dict[str, Any]:
    keys = {
        "LLVM_SYS_181_PREFIX",
        "LLVM_CONFIG_PATH",
        "RUSTC",
        "RUSTFLAGS",
        "CARGO_BUILD_JOBS",
        "CARGO_TARGET_DIR",
        "CARGO_INCREMENTAL",
        "CC",
        "CXX",
        "AR",
        "LD",
        "LD_LIBRARY_PATH",
        "ASAN_OPTIONS",
        "LSAN_OPTIONS",
        "TSAN_OPTIONS",
        "UBSAN_OPTIONS",
        "MIMI_OPT",
        "MIMI_PROFILE",
        "MIMI_ROUTE",
    }
    keys.update(k for k in os.environ if k.startswith(("MIMI_", "CARGO_PROFILE_", "CARGO_TARGET_")))
    result: dict[str, Any] = {}
    for key in sorted(keys):
        if key not in os.environ:
            continue
        if any(token in key.upper() for token in ("TOKEN", "PASSWORD", "SECRET", "API_KEY", "PRIVATE_KEY")):
            result[key] = "<redacted>"
        else:
            result[key] = os.environ[key]
    return result


def resolve_executable(value: str) -> Path | None:
    candidate = Path(value)
    if candidate.is_file() and os.access(candidate, os.X_OK):
        return candidate.resolve()
    resolved = shutil.which(value)
    return Path(resolved).resolve() if resolved else None


def probe_tool(name: str, command: Sequence[str], extra_commands: Sequence[Sequence[str]] = ()) -> dict[str, Any]:
    executable = resolve_executable(command[0]) if command else None
    record: dict[str, Any] = {"name": name, "command": list(command), "status": "missing"}
    if executable is None:
        return record
    record["executable"] = file_record(executable, executable.parent.parent if executable.parent.name == "bin" else executable.parent)
    try:
        process = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False, timeout=20)
        output = process.stdout.decode(errors="replace")
        record.update(
            {
                "status": "observed",
                "exit_code": process.returncode,
                "version_output": output,
                "version_output_sha256": sha256_bytes(process.stdout),
            }
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        record.update({"status": "probe-failed", "error": str(error)})
    details: list[dict[str, Any]] = []
    for argv in extra_commands:
        try:
            result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False, timeout=20)
            details.append(
                {
                    "command": list(argv),
                    "exit_code": result.returncode,
                    "output": result.stdout.decode(errors="replace"),
                    "output_sha256": sha256_bytes(result.stdout),
                }
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            details.append({"command": list(argv), "status": "probe-failed", "error": str(error)})
    if details:
        record["details"] = details
    return record


def toolchain_snapshot() -> dict[str, Any]:
    llvm_prefix = os.environ.get("LLVM_SYS_181_PREFIX")
    llvm_config = os.environ.get("LLVM_CONFIG_PATH")
    if llvm_config:
        llvm_command = [llvm_config, "--version"]
        llvm_extra = [[llvm_config, flag] for flag in ("--bindir", "--libdir", "--cflags", "--libs")]
    elif llvm_prefix:
        llvm_command = [str(Path(llvm_prefix) / "bin/llvm-config"), "--version"]
        llvm_extra = [[llvm_command[0], flag] for flag in ("--bindir", "--libdir", "--cflags", "--libs")]
    else:
        llvm_command = ["llvm-config-18", "--version"]
        llvm_extra = [["llvm-config-18", flag] for flag in ("--bindir", "--libdir", "--cflags", "--libs")]
        if resolve_executable("llvm-config-18") is None:
            llvm_command = ["llvm-config", "--version"]
            llvm_extra = [["llvm-config", flag] for flag in ("--bindir", "--libdir", "--cflags", "--libs")]
    tools = {
        "rustc": probe_tool("rustc", [os.environ.get("RUSTC", "rustc"), "--version", "--verbose"]),
        "cargo": probe_tool("cargo", ["cargo", "--version"]),
        "llvm": probe_tool("llvm", llvm_command, llvm_extra),
        "z3": probe_tool("z3", ["z3", "--version"]),
        "cc": probe_tool("cc", [os.environ.get("CC", "cc"), "--version"]),
        "valgrind": probe_tool("valgrind", ["valgrind", "--version"]),
        "time": probe_tool("time", ["/usr/bin/time", "--version"]),
        "perf": probe_tool("perf", ["perf", "--version"]),
        "heaptrack": probe_tool("heaptrack", ["heaptrack", "--version"]),
    }
    rustc = resolve_executable(os.environ.get("RUSTC", "rustc"))
    if rustc:
        try:
            sysroot = subprocess.run(
                [str(rustc), "--print", "sysroot"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=20
            )
            if sysroot.returncode == 0:
                toolchain_rustc = Path(sysroot.stdout.decode(errors="replace").strip()) / "bin/rustc"
                if toolchain_rustc.is_file():
                    tools["rustc"]["toolchain_executable"] = file_record(toolchain_rustc, toolchain_rustc.parent.parent)
        except (OSError, subprocess.TimeoutExpired):
            pass
    python_z3: dict[str, Any] = {"status": "unknown", "reason": "python z3 probe not requested"}
    try:
        probe = subprocess.run(
            [sys.executable, "-c", "import z3; print(getattr(z3, '__version__', 'unknown'))"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
            timeout=20,
        )
        python_z3 = {
            "status": "observed" if probe.returncode == 0 else "probe-failed",
            "exit_code": probe.returncode,
            "output": probe.stdout.decode(errors="replace"),
            "output_sha256": sha256_bytes(probe.stdout),
        }
    except (OSError, subprocess.TimeoutExpired) as error:
        python_z3 = {"status": "probe-failed", "error": str(error)}
    return {
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "version": platform.version(),
            "machine": platform.machine(),
            "processor": platform.processor(),
            "python": platform.python_version(),
        },
        "tools": tools,
        "python_z3": python_z3,
        "environment": environment_snapshot(),
    }


def host_snapshot() -> dict[str, Any]:
    result: dict[str, Any] = {
        "cpu_count": os.cpu_count(),
        "page_size": None,
        "memory_total_bytes": None,
        "memory_source": None,
        "load_average": None,
    }
    try:
        result["page_size"] = os.sysconf("SC_PAGE_SIZE")
    except (ValueError, OSError, AttributeError):
        pass
    try:
        result["memory_total_bytes"] = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
        result["memory_source"] = "sysconf"
    except (ValueError, OSError, AttributeError):
        try:
            for line in Path("/proc/meminfo").read_text().splitlines():
                if line.startswith("MemTotal:"):
                    result["memory_total_bytes"] = int(line.split()[1]) * 1024
                    result["memory_source"] = "/proc/meminfo"
                    break
        except (OSError, ValueError):
            pass
    try:
        result["load_average"] = list(os.getloadavg())
    except (OSError, AttributeError):
        pass
    for filename in ("/proc/cpuinfo", "/proc/meminfo"):
        path = Path(filename)
        if path.is_file():
            try:
                result[Path(filename).name + "_sha256"] = sha256_file(path)
            except OSError:
                pass
    return result


def artifact_record(path: Path, root: Path) -> dict[str, Any]:
    result = {"path": relative(path, root), "absolute_path": str(path.resolve())}
    if not path.is_file():
        result.update({"status": "missing", "sha256": None, "bytes": None})
        return result
    try:
        result.update({"status": "observed", "sha256": sha256_file(path), "bytes": path.stat().st_size})
    except OSError as error:
        result.update({"status": "unreadable", "error": str(error), "sha256": None, "bytes": None})
    return result


def command_output_class(text: str, exit_code: int | None, expected_exit: int = 0, expected_class: Any = None) -> str:
    if expected_class is not None and exit_code == expected_exit:
        return normalize_result_class(expected_class)
    if exit_code is None:
        return "环境未验证"
    if exit_code == expected_exit:
        return "已通过"
    lowered = text.lower()
    if any(token in lowered for token in ("unsupported", "not supported", "mir-coverage-001", "e0722")):
        return "不支持"
    if any(token in lowered for token in ("type error", "parse error", "rejected", "diagnostic", "error[e")):
        return "明确拒绝"
    return "未解释失败"


def classify_route(text: str) -> dict[str, Any]:
    lowered = text.lower()
    candidates: list[str] = []
    if re.search(r"\bcanonical\b|mir[-_ ]route|route[-_ ]receipt", lowered):
        candidates.append("canonical")
    if re.search(r"compatibility|compat\b|legacy", lowered):
        candidates.append("compatibility")
    if re.search(r"mir-coverage-001|notinsubset|unsupported|not supported", lowered):
        candidates.append("rejected-or-unsupported")
    observed = candidates[0] if candidates else None
    return {"observed": observed, "candidates": candidates, "status": "observed" if observed else "unknown"}


def classify_backend(text: str) -> dict[str, Any]:
    lowered = text.lower()
    candidates: list[str] = []
    for label, patterns in {
        "reference": ("reference", "ref-executor"),
        "bytecode": ("bytecode", "vm", "interp"),
        "native": ("native", "llvm", "codegen"),
        "mir": ("canonical mir", "mir route", "mir verifier"),
        "legacy": ("legacy", "compatibility"),
    }.items():
        if any(pattern in lowered for pattern in patterns):
            candidates.append(label)
    return {"observed": candidates[0] if candidates else None, "candidates": candidates,
            "status": "observed" if candidates else "unknown"}


VERIFIER_STATES = (
    "Proven",
    "Disproven",
    "NotInTrustedSubset",
    "SolverUnknown",
    "Timeout",
    "InfrastructureError",
    "RuntimeOnlyContract",
    "NoObligations",
)


def classify_verifier(text: str) -> dict[str, Any]:
    found: list[str] = []
    for state in VERIFIER_STATES:
        if re.search(rf"\b{re.escape(state)}\b", text, re.IGNORECASE):
            found.append(state)
    return {"observed": found[0] if found else None, "states": found,
            "status": "observed" if found else "unknown"}


def classify_emitter_eligibility(text: str, receipt: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Extract eligibility only when a receipt/log states it explicitly."""

    match = re.search(r"(?:emitter[_ -]?eligib(?:ility|le)|eligible)\s*[:=]\s*(true|false|yes|no)", text, re.IGNORECASE)
    if match:
        return {"status": "observed", "eligible": match.group(1).lower() in ("true", "yes"), "source": "log"}
    if receipt is not None:
        values = recursive_find(receipt, {"emitter_eligibility", "emitter_eligible", "eligible"})
        if values:
            value = values[0]
            if isinstance(value, bool):
                return {"status": "observed", "eligible": value, "source": "receipt"}
            if isinstance(value, str) and value.lower() in ("true", "false", "yes", "no"):
                return {"status": "observed", "eligible": value.lower() in ("true", "yes"), "source": "receipt"}
    return {"status": "unknown", "eligible": None, "reason": "No machine-readable per-function emitter eligibility was emitted."}


def parse_receipt(text: str) -> dict[str, Any] | None:
    """Best-effort parse of ``mimi mir --receipt`` output.

    The CLI has emitted both a JSON object and a JSON array across development
    versions.  Keep the raw log as the authority when output is not JSON.
    """

    for candidate in (text.strip(),):
        if not candidate:
            continue
        try:
            value = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
        if isinstance(value, list):
            return {"entries": value}
    return None


def recursive_find(value: Any, names: set[str]) -> list[Any]:
    found: list[Any] = []
    if isinstance(value, dict):
        for key, item in value.items():
            if key.lower() in names:
                found.append(item)
            found.extend(recursive_find(item, names))
    elif isinstance(value, list):
        for item in value:
            found.extend(recursive_find(item, names))
    return found


def observed_from_receipt(receipt: Mapping[str, Any] | None) -> dict[str, Any]:
    if receipt is None:
        return {"status": "unknown"}
    route = recursive_find(receipt, {"route", "route_kind", "route_name", "profile"})
    backend = recursive_find(receipt, {"backend", "consumer", "execution_backend"})
    verifier = recursive_find(receipt, {"verifier", "verification", "verifier_status"})
    return {"status": "observed", "route": route, "backend": backend, "verifier": verifier}


def child_usage(before: resource.struct_rusage, after: resource.struct_rusage) -> dict[str, Any]:
    # ru_maxrss is a process maximum, not a delta.  Keep it as a maximum and
    # report CPU/block/switch fields as deltas where the OS exposes them.
    max_rss = getattr(after, "ru_maxrss", None)
    rss_unit = "KiB" if platform.system() != "Darwin" else "bytes"
    return {
        "peak_rss": {"value": max_rss, "unit": rss_unit, "status": "observed" if max_rss is not None else "unknown",
                     "source": "resource.RUSAGE_CHILDREN.ru_maxrss", "scope": "cumulative children unless GNU time wrapper overrides"},
        "user_cpu_seconds": getattr(after, "ru_utime", 0.0) - getattr(before, "ru_utime", 0.0),
        "system_cpu_seconds": getattr(after, "ru_stime", 0.0) - getattr(before, "ru_stime", 0.0),
        "minor_faults": getattr(after, "ru_minflt", 0) - getattr(before, "ru_minflt", 0),
        "major_faults": getattr(after, "ru_majflt", 0) - getattr(before, "ru_majflt", 0),
        "voluntary_context_switches": getattr(after, "ru_nvcsw", 0) - getattr(before, "ru_nvcsw", 0),
        "involuntary_context_switches": getattr(after, "ru_nivcsw", 0) - getattr(before, "ru_nivcsw", 0),
        "read_io": getattr(after, "ru_inblock", 0) - getattr(before, "ru_inblock", 0),
        "write_io": getattr(after, "ru_oublock", 0) - getattr(before, "ru_oublock", 0),
    }


def refine_gnu_time_usage(usage: dict[str, Any], stderr: str) -> dict[str, Any]:
    """Use GNU time's per-child peak when the command was wrapped with -v."""

    match = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr)
    if match:
        usage["peak_rss"] = {
            "value": int(match.group(1)),
            "unit": "KiB",
            "status": "observed",
            "source": "/usr/bin/time -v",
            "scope": "this command",
        }
    return usage


@dataclass(frozen=True)
class CommandSpec:
    fixture: str
    command_id: str
    argv: tuple[str, ...]
    phase: str
    route: str
    backend: str
    verifier: str
    expected_exit: int = 0
    expected_class: str | None = None
    timeout_seconds: float = 120.0
    artifact: str | None = None
    memory_tool: str = "none"
    allocation_tool: str = "none"
    repetitions: int = 1
    source_path: str | None = None


def validate_id(value: str, label: str) -> str:
    if not isinstance(value, str) or not IDENTIFIER.fullmatch(value):
        raise ValueError(f"{label} must use letters, digits, dots, dashes or underscores")
    return value


def fixture_schema() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "fixtures": [
            {
                "id": "dsp",
                "source": "benchmarks/dsp.mimi",
                "commands": [
                    {
                        "id": "check-default",
                        "argv": ["{mimi}", "check", "{source}"],
                        "phase": "check",
                        "route": "default",
                        "backend": "checker",
                        "verifier": "none",
                        "expected_exit": 0,
                    }
                ],
            }
        ],
    }


def default_specs(mimi: Path, source: Path, fixture_id: str, artifact_dir: Path) -> list[CommandSpec]:
    source_text = str(source)
    artifact_default = artifact_dir / f"{fixture_id}.native"
    return [
        CommandSpec(fixture_id, "check-default", (str(mimi), "check", source_text), "check", "default", "checker", "none", source_path=source_text),
        CommandSpec(fixture_id, "mir-receipt", (str(mimi), "mir", "--receipt", source_text), "route", "direct", "mir", "none", source_path=source_text),
        CommandSpec(fixture_id, "run-default", (str(mimi), "run", source_text), "run", "default", "bytecode", "none", source_path=source_text),
        CommandSpec(fixture_id, "run-mir", (str(mimi), "run", "--mir", source_text), "run", "default", "mir-vm", "none", source_path=source_text),
        CommandSpec(fixture_id, "verify-default", (str(mimi), "verify", "--stats", source_text), "verify", "default", "verifier", "z3", source_path=source_text),
        CommandSpec(fixture_id, "verify-mir", (str(mimi), "verify", "--mir", "--stats", source_text), "verify", "direct", "mir-verifier", "z3", source_path=source_text),
        CommandSpec(fixture_id, "build-default", (str(mimi), "build", "-o", str(artifact_default), source_text), "compile", "default", "native", "none", artifact=str(artifact_default), memory_tool="time", source_path=source_text),
        CommandSpec(fixture_id, "build-mir", (str(mimi), "build", "--mir", "-o", str(artifact_dir / f"{fixture_id}.mir.native"), source_text), "compile", "direct", "native-mir", "none", artifact=str(artifact_dir / f"{fixture_id}.mir.native"), memory_tool="time", source_path=source_text),
        CommandSpec(fixture_id, "run-native-default", (str(artifact_default),), "run", "default", "native", "none", artifact=str(artifact_default), memory_tool="time", source_path=source_text),
        CommandSpec(fixture_id, "run-native-mir", (str(artifact_dir / f"{fixture_id}.mir.native"),), "run", "direct", "native-mir", "none", artifact=str(artifact_dir / f"{fixture_id}.mir.native"), memory_tool="time", source_path=source_text),
    ]


def load_specs(path: Path, root: Path, mimi: Path, evidence: Path, selected_sources: set[str] | None) -> tuple[list[CommandSpec], list[dict[str, Any]]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != SCHEMA_VERSION:
        raise ValueError("fixture manifest schema_version must be 1")
    fixtures = data.get("fixtures")
    if not isinstance(fixtures, list) or not fixtures:
        raise ValueError("fixture manifest needs a non-empty fixtures array")
    specs: list[CommandSpec] = []
    source_records: list[dict[str, Any]] = []
    for fixture in fixtures:
        if not isinstance(fixture, dict):
            raise ValueError("fixture entries must be objects")
        fixture_id = validate_id(fixture.get("id"), "fixture id")
        source_value = fixture.get("source")
        if not isinstance(source_value, str) or not source_value:
            raise ValueError(f"fixture {fixture_id} source is required")
        source = Path(source_value)
        if not source.is_absolute():
            source = root / source
        source_key = relative(source, root)
        if selected_sources and fixture_id not in selected_sources and source_key not in selected_sources and str(source) not in selected_sources:
            continue
        source_records.append({"fixture": fixture_id, "source": source_key, "input_kind": source.suffix.lstrip(".") or "unknown"})
        commands = fixture.get("commands")
        if not isinstance(commands, list) or not commands:
            raise ValueError(f"fixture {fixture_id} commands must be a non-empty array")
        seen: set[str] = set()
        for command in commands:
            if not isinstance(command, dict):
                raise ValueError(f"fixture {fixture_id} command must be an object")
            command_id = validate_id(command.get("id"), f"fixture {fixture_id} command id")
            if command_id in seen:
                raise ValueError(f"duplicate command id: {fixture_id}/{command_id}")
            seen.add(command_id)
            argv_value = command.get("argv", command.get("command"))
            if not isinstance(argv_value, list) or not argv_value or any(not isinstance(item, str) for item in argv_value):
                raise ValueError(f"fixture {fixture_id}/{command_id} argv must be a non-empty string array")
            context = {
                "mimi": str(mimi),
                "source": str(source),
                "artifact": str(evidence / "artifacts" / f"{fixture_id}-{command_id}"),
                "root": str(root),
                "evidence": str(evidence),
            }
            argv = tuple(item.format(**context) for item in argv_value)
            artifact_value = command.get("artifact")
            artifact_path = None
            if artifact_value:
                artifact_path = str(Path(str(artifact_value).format(**context)))
            expected_class = command.get("expected_class", command.get("expected", {}).get("result_class") if isinstance(command.get("expected"), dict) else None)
            specs.append(CommandSpec(
                fixture_id,
                command_id,
                argv,
                str(command.get("phase", "probe")),
                str(command.get("route", "unknown")),
                str(command.get("backend", "unknown")),
                str(command.get("verifier", "unknown")),
                int(command.get("expected_exit", command.get("expected", {}).get("exit_code", 0) if isinstance(command.get("expected"), dict) else 0)),
                normalize_result_class(expected_class) if expected_class is not None else None,
                float(command.get("timeout_seconds", 120.0)),
                artifact_path,
                str(command.get("memory_tool", "none")),
                str(command.get("allocation_tool", "none")),
                int(command.get("repetitions", 1)),
                source_key,
            ))
    if not specs:
        raise ValueError("fixture selection produced no commands")
    return specs, source_records


def expand_templates(argv: Sequence[str], context: Mapping[str, str]) -> list[str]:
    return [item.format(**context) for item in argv]


def wrap_memory_command(argv: Sequence[str], memory_tool: str) -> tuple[list[str], dict[str, Any]]:
    tool = memory_tool.strip().lower()
    if tool in ("", "none", "unknown"):
        return list(argv), {"requested": memory_tool or "none", "status": "not-requested"}
    if tool == "time":
        executable = Path("/usr/bin/time")
        if not executable.is_file():
            return list(argv), {"requested": tool, "status": "unknown", "reason": "/usr/bin/time is missing"}
        return [str(executable), "-v", "--", *argv], {"requested": tool, "status": "wrapped"}
    if tool == "valgrind":
        executable = resolve_executable("valgrind")
        if executable is None:
            return list(argv), {"requested": tool, "status": "unknown", "reason": "valgrind is missing"}
        return [str(executable), "--tool=memcheck", "--leak-check=full", "--error-exitcode=99", *argv], {"requested": tool, "status": "wrapped"}
    if tool in ("massif", "heaptrack"):
        executable = resolve_executable(tool)
        if executable is None:
            return list(argv), {"requested": tool, "status": "unknown", "reason": f"{tool} is missing"}
        return [str(executable), *argv], {"requested": tool, "status": "wrapped"}
    return list(argv), {"requested": tool, "status": "unknown", "reason": "unsupported memory tool"}


def allocation_observation(stderr: str, allocation_tool: str, wrapped: Mapping[str, Any]) -> dict[str, Any]:
    tool = allocation_tool.strip().lower()
    if tool in ("", "none", "unknown"):
        return {
            "status": "unknown",
            "reason": "No allocator instrumentation was requested; peak RSS is not an allocation count.",
            "tool": tool or "none",
        }
    if tool == "valgrind":
        match = re.search(r"ERROR SUMMARY:\s*(\d+) errors?", stderr)
        if match:
            return {"status": "observed", "tool": tool, "errors": int(match.group(1)), "raw_match": match.group(0)}
        return {"status": "unknown", "tool": tool, "reason": "Valgrind summary was not found in stderr."}
    if tool == "massif":
        return {"status": "unknown", "tool": tool, "reason": "Massif output path was not declared; allocation bytes are unavailable."}
    if tool == "heaptrack":
        return {"status": "unknown", "tool": tool, "reason": "Heaptrack output parsing is not enabled in this collector."}
    if wrapped.get("status") != "wrapped":
        return {"status": "unknown", "tool": tool, "reason": str(wrapped.get("reason", "allocation wrapper unavailable"))}
    return {"status": "unknown", "tool": tool, "reason": "Allocation parser has no result for this tool."}


def run_one(spec: CommandSpec, root: Path, evidence: Path, source_identity: str, iteration: int, warmup: bool, source_before: Mapping[str, Any]) -> dict[str, Any]:
    context = {
        "root": str(root),
        "evidence": str(evidence),
        "mimi": spec.argv[0] if spec.argv else "",
        "source": next((item for item in spec.argv if item.endswith(".mimi")), ""),
        "artifact": spec.artifact or "",
    }
    argv = expand_templates(spec.argv, context)
    wrapped_argv, memory_meta = wrap_memory_command(argv, spec.memory_tool)
    stem = f"{spec.fixture}-{spec.command_id}-{'warmup' if warmup else f'run{iteration:02d}'}"
    safe_stem = re.sub(r"[^A-Za-z0-9_.-]+", "_", stem)
    stdout_path = evidence / "logs" / f"{safe_stem}.stdout"
    stderr_path = evidence / "logs" / f"{safe_stem}.stderr"
    record_path = evidence / "records" / f"{safe_stem}.json"
    source_after: dict[str, Any] | None = None
    start_wall = time.time()
    start_mono = time.monotonic()
    before_usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    launch_error: str | None = None
    timed_out = False
    exit_code: int | None = None
    out = b""
    err = b""
    try:
        process = subprocess.Popen(
            wrapped_argv,
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            env=os.environ.copy(),
        )
        try:
            out, err = process.communicate(timeout=spec.timeout_seconds)
            exit_code = process.returncode
        except subprocess.TimeoutExpired as error:
            timed_out = True
            out = error.stdout or b""
            err = error.stderr or b""
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except OSError:
                process.terminate()
            try:
                extra_out, extra_err = process.communicate(timeout=5)
                out += extra_out or b""
                err += extra_err or b""
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except OSError:
                    process.kill()
                extra_out, extra_err = process.communicate()
                out += extra_out or b""
                err += extra_err or b""
            exit_code = None
    except OSError as error:
        launch_error = str(error)
        err = launch_error.encode()
    end_mono = time.monotonic()
    end_wall = time.time()
    after_usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    usage = child_usage(before_usage, after_usage) if launch_error is None else {
        "peak_rss": {"value": None, "unit": "KiB", "status": "unknown", "reason": "command did not launch"},
        "user_cpu_seconds": None,
        "system_cpu_seconds": None,
        "allocation": {"status": "unknown", "reason": "command did not launch"},
    }
    usage = refine_gnu_time_usage(usage, err.decode(errors="replace"))
    text_for_class = (out + b"\n" + err).decode(errors="replace")
    result_class = "环境未验证" if timed_out or launch_error else command_output_class(text_for_class, exit_code, spec.expected_exit, spec.expected_class)
    if warmup:
        result_class = "未运行" if launch_error is None and exit_code == spec.expected_exit else result_class
    selected_source = spec.source_path or source_before.get("selected_source", "")
    identity_inputs = source_before.get("inputs") or ([selected_source] if selected_source else [])
    source_after = source_snapshot(root, identity_inputs) if identity_inputs else None
    source_stable = source_after is None or source_after.get("source_identity") == source_identity
    if not source_stable and result_class == "已通过":
        result_class = "未解释失败"
    receipt = parse_receipt(out.decode(errors="replace"))
    route_observed = classify_route(text_for_class)
    backend_observed = classify_backend(text_for_class)
    verifier_observed = classify_verifier(text_for_class)
    emitter_observed = classify_emitter_eligibility(text_for_class, receipt)
    if receipt is not None:
        route_observed["receipt"] = observed_from_receipt(receipt)
    if launch_error is not None:
        err += ("\nlaunch_error=" + launch_error).encode()
    text_write_exclusive(stdout_path, out)
    text_write_exclusive(stderr_path, err)
    allocation = allocation_observation(err.decode(errors="replace"), spec.allocation_tool, memory_meta)
    usage["allocation"] = allocation
    record: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": EVIDENCE_KIND,
        "fixture_id": spec.fixture,
        "command_id": spec.command_id,
        "source_path": selected_source or None,
        "input_kind": Path(selected_source).suffix.lstrip(".") if selected_source else "unknown",
        "iteration": iteration,
        "warmup": warmup,
        "phase": spec.phase,
        "command": argv,
        "wrapped_command": wrapped_argv,
        "cwd": str(root),
        "source_identity": source_identity,
        "source_stable": source_stable,
        "expected_exit": spec.expected_exit,
        "expected_exit_match": exit_code == spec.expected_exit,
        "expected_result_class": spec.expected_class,
        "exit_code": exit_code,
        "launch_error": launch_error,
        "timed_out": timed_out,
        "started_at_unix": start_wall,
        "ended_at_unix": end_wall,
        "elapsed_seconds": end_mono - start_mono,
        "route": {"intent": spec.route, **route_observed},
        "backend": {"intent": spec.backend, **backend_observed},
        "verifier": {"intent": spec.verifier, **verifier_observed},
        "emitter_eligibility": emitter_observed,
        "memory": {"wrapper": memory_meta, **usage},
        "allocation": allocation,
        "copy_events": {"status": "unknown", "reason": "No copy/move event instrumentation was requested."},
        "result_class": result_class,
        "accepted": exit_code == spec.expected_exit and result_class in ("已通过", "明确拒绝") and not warmup,
        "raw_logs": {
            "stdout": {"path": relative(stdout_path, evidence), "sha256": sha256_file(stdout_path), "bytes": len(out)},
            "stderr": {"path": relative(stderr_path, evidence), "sha256": sha256_file(stderr_path), "bytes": len(err)},
        },
        "limitations": [
            "Peak RSS is process-wide child maximum and is not an allocation count.",
            "Allocator/copy metrics remain unknown unless an instrumentation tool is explicitly requested and parsed.",
            "Observed route/backend fields are best-effort text/receipt extraction; intent is preserved separately.",
        ],
    }
    artifact_ok = True
    if spec.artifact:
        artifact = Path(spec.artifact)
        observed_artifact = artifact
        # Build commands commonly reuse one ``-o`` path across repetitions.
        # Snapshot each successful product before the next repetition can
        # overwrite it, so old evidence remains verifiable.
        if spec.phase in ("compile", "build") and artifact.is_file():
            snapshot_path = evidence / "artifacts" / f"{safe_stem}.artifact"
            try:
                shutil.copy2(artifact, snapshot_path)
                observed_artifact = snapshot_path
                record["artifact_source_path"] = str(artifact)
            except OSError as error:
                record["artifact_snapshot_error"] = str(error)
                artifact_ok = False
        record["artifact"] = artifact_record(observed_artifact, evidence)
        artifact_ok = record["artifact"].get("status") == "observed"
        if not artifact_ok and result_class == "已通过":
            result_class = "未解释失败"
            record["artifact_requirement_failure"] = "declared native artifact was not produced"
    record["result_class"] = result_class
    record["accepted"] = exit_code == spec.expected_exit and artifact_ok and result_class in ("已通过", "明确拒绝") and not warmup
    record["record_content_hash"] = digest_json({k: v for k, v in record.items() if k != "record_content_hash"})
    json_write_exclusive(record_path, record)
    return record


def quantile(values: Sequence[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    index = (len(ordered) - 1) * q
    lower = math.floor(index)
    upper = math.ceil(index)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (index - lower)


def spec_record(spec: CommandSpec) -> dict[str, Any]:
    return {
        "fixture_id": spec.fixture,
        "command_id": spec.command_id,
        "argv": list(spec.argv),
        "phase": spec.phase,
        "route": spec.route,
        "backend": spec.backend,
        "verifier": spec.verifier,
        "expected_exit": spec.expected_exit,
        "expected_class": spec.expected_class,
        "timeout_seconds": spec.timeout_seconds,
        "artifact": spec.artifact,
        "memory_tool": spec.memory_tool,
        "allocation_tool": spec.allocation_tool,
        "repetitions": spec.repetitions,
        "source_path": spec.source_path,
    }


def aggregate(records: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    counts = {label: 0 for label in sorted(RESULT_CLASSES)}
    by_fixture: dict[str, dict[str, int]] = {}
    elapsed: list[float] = []
    rss: list[float] = []
    compile_elapsed: list[float] = []
    run_elapsed: list[float] = []
    install_elapsed: list[float] = []
    emitter_counts = {"eligible": 0, "ineligible": 0, "unknown": 0}
    for row in records:
        label = normalize_result_class(row.get("result_class"), default="未运行")
        counts[label] += 1
        fixture = str(row.get("fixture_id", "unknown"))
        by_fixture.setdefault(fixture, {name: 0 for name in sorted(RESULT_CLASSES)})[label] += 1
        if not row.get("warmup") and isinstance(row.get("elapsed_seconds"), (float, int)):
            elapsed.append(float(row["elapsed_seconds"]))
            phase = str(row.get("phase", ""))
            if phase in ("compile", "build"):
                compile_elapsed.append(float(row["elapsed_seconds"]))
            if phase == "run":
                run_elapsed.append(float(row["elapsed_seconds"]))
            if phase == "install":
                install_elapsed.append(float(row["elapsed_seconds"]))
        value = row.get("memory", {}).get("peak_rss", {}).get("value") if isinstance(row.get("memory"), dict) else None
        if isinstance(value, (float, int)):
            rss.append(float(value))
        eligibility = row.get("emitter_eligibility", {})
        if isinstance(eligibility, dict) and eligibility.get("status") == "observed":
            emitter_counts["eligible" if eligibility.get("eligible") else "ineligible"] += 1
        else:
            emitter_counts["unknown"] += 1
    return {
        "result_counts": counts,
        "by_fixture": by_fixture,
        "samples": len(records),
        "non_warmup_samples": len(elapsed),
        "elapsed_seconds": {"p50": quantile(elapsed, 0.50), "p95": quantile(elapsed, 0.95), "max": max(elapsed) if elapsed else None,
                             "mean": statistics.mean(elapsed) if elapsed else None,
                             "stdev": statistics.stdev(elapsed) if len(elapsed) > 1 else None},
        "compile_seconds": {"p50": quantile(compile_elapsed, 0.50), "p95": quantile(compile_elapsed, 0.95), "samples": len(compile_elapsed)},
        "run_seconds": {"p50": quantile(run_elapsed, 0.50), "p95": quantile(run_elapsed, 0.95), "samples": len(run_elapsed)},
        "install_seconds": {"p50": quantile(install_elapsed, 0.50), "p95": quantile(install_elapsed, 0.95), "samples": len(install_elapsed)},
        "peak_rss": {"p50": quantile(rss, 0.50), "p95": quantile(rss, 0.95), "max": max(rss) if rss else None,
                      "unit": "KiB", "status": "observed" if rss else "unknown"},
        "emitter_eligibility": {"counts": emitter_counts, "status": "observed" if emitter_counts["eligible"] + emitter_counts["ineligible"] else "unknown"},
        "allocation": {"status": "unknown", "reason": "No aggregate allocation counter is inferred from elapsed/RSS samples."},
        "copy_events": {"status": "unknown", "reason": "No aggregate copy/move counter is inferred from elapsed/RSS samples."},
        "throughput": {"status": "observed" if elapsed else "unknown", "commands_per_second_p50": (1.0 / quantile(elapsed, 0.50) if quantile(elapsed, 0.50) and quantile(elapsed, 0.50) > 0 else None)},
    }


def static_census(root: Path, source_paths: Sequence[Path]) -> dict[str, Any]:
    """Record inexpensive source owner hints without claiming route coverage."""

    owner_patterns = {
        "CodegenLegacyRemainder": ("compile_func_legacy", "compile_generic_func"),
        "FlowVerifierCompatibility": ("FlowVerifierCompatibility", "legacy_flow"),
        "FfiVerifierCompatibility": ("FfiVerifierCompatibility", "legacy_ffi"),
        "DualVerifierCompatibility": ("DualVerifierCompatibility", "verify_dual_legacy"),
    }
    files = [path for path in tracked_paths(root) if path.suffix in {".rs", ".mimi", ".toml"}]
    result: dict[str, Any] = {"status": "observed", "files_scanned": len(files), "owners": {}}
    for owner, patterns in owner_patterns.items():
        matches: list[dict[str, Any]] = []
        for path in files:
            try:
                text = path.read_text(errors="replace")
            except OSError:
                continue
            for pattern in patterns:
                count = text.count(pattern)
                if count:
                    matches.append({"path": relative(path, root), "pattern": pattern, "count": count})
        result["owners"][owner] = {"matches": matches, "status": "observed"}
    result["limitations"] = [
        "Text owner census is a static hint, not a proof of production reachability or whole-program route coverage.",
        "Use the retained command logs and route receipts for executable evidence.",
    ]
    return result


def executable_census(records: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    """Summarize command results without promoting unknowns to coverage."""

    by_input: dict[str, dict[str, Any]] = {}
    for row in records:
        if row.get("warmup"):
            continue
        fixture = str(row.get("fixture_id", "unknown"))
        bucket = by_input.setdefault(fixture, {
            "input_kind": row.get("input_kind", "unknown"),
            "whole_program": {"canonical": 0, "compatibility": 0, "rejected": 0, "build_failure": 0, "environment_failure": 0, "unknown": 0},
            "per_function_emitter_eligibility": {"eligible": 0, "ineligible": 0, "unknown": 0},
            "records": 0,
        })
        bucket["records"] += 1
        route = row.get("route", {}) if isinstance(row.get("route"), dict) else {}
        observed = route.get("observed")
        result_class = row.get("result_class")
        if observed == "canonical":
            bucket["whole_program"]["canonical"] += 1
        elif observed == "compatibility":
            bucket["whole_program"]["compatibility"] += 1
        elif observed == "rejected-or-unsupported" or result_class in ("明确拒绝", "不支持"):
            bucket["whole_program"]["rejected"] += 1
        elif result_class == "环境未验证":
            bucket["whole_program"]["environment_failure"] += 1
        elif result_class == "未解释失败":
            bucket["whole_program"]["build_failure"] += 1
        else:
            bucket["whole_program"]["unknown"] += 1
        eligibility = row.get("emitter_eligibility", {})
        if isinstance(eligibility, dict) and eligibility.get("status") == "observed":
            bucket["per_function_emitter_eligibility"]["eligible" if eligibility.get("eligible") else "ineligible"] += 1
        else:
            bucket["per_function_emitter_eligibility"]["unknown"] += 1
    return {
        "status": "observed" if by_input else "unknown",
        "by_input": by_input,
        "limitations": [
            "Counts are per declared command, not a proof that every callable in the source was reached.",
            "A canonical command with an unknown receipt remains unknown rather than being counted as canonical.",
        ],
    }


def source_inputs_from_args(root: Path, sources: Sequence[str], fixture_path: Path | None) -> list[str]:
    values = list(sources)
    if fixture_path:
        values.append(str(fixture_path))
    values.append(str(SCRIPT.relative_to(root)) if SCRIPT.is_relative_to(root) else str(SCRIPT))
    return values


def command_matrix_for_args(args: argparse.Namespace, root: Path, evidence: Path) -> tuple[list[CommandSpec], list[dict[str, Any]], dict[str, Any] | None]:
    mimi = Path(args.mimi)
    if not mimi.is_absolute():
        candidate = root / mimi
        mimi = candidate if candidate.exists() else Path(args.mimi)
    if args.fixture:
        fixture_path = Path(args.fixture)
        if not fixture_path.is_absolute():
            fixture_path = root / fixture_path
        selected = set(args.source) if args.source else None
        specs, fixture_records = load_specs(fixture_path, root, mimi, evidence, selected)
        return specs, fixture_records, json.loads(fixture_path.read_text(encoding="utf-8"))
    sources = list(args.source)
    if not sources:
        sources = ["benchmarks/dsp.mimi"]
    specs: list[CommandSpec] = []
    fixture_records: list[dict[str, Any]] = []
    for index, value in enumerate(sources):
        source = Path(value)
        if not source.is_absolute():
            source = root / source
        fixture_id = validate_id(Path(value).stem or f"fixture-{index}", "fixture id")
        fixture_records.append({"fixture": fixture_id, "source": relative(source, root), "input_kind": source.suffix.lstrip(".") or "unknown"})
        specs.extend(default_specs(mimi, source, fixture_id, evidence / "artifacts"))
    return specs, fixture_records, None


def snapshot_mode(args: argparse.Namespace) -> int:
    root = Path(args.root).resolve()
    if args.evidence_dir is None:
        raise ValueError("snapshot requires --evidence-dir")
    evidence = Path(args.evidence_dir).resolve()
    if (evidence / "manifest.json").exists() or (evidence / "snapshot.json").exists():
        raise RuntimeError(f"evidence directory already has a manifest: {evidence}")
    evidence.mkdir(parents=True, exist_ok=True)
    inputs = source_inputs_from_args(root, args.source, None)
    source = source_snapshot(root, inputs)
    data = {
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": EVIDENCE_KIND,
        "created_at_utc": datetime.now(timezone.utc).isoformat(),
        "task_ids": ["B-01", "B-04", "B-08"],
        "source": source,
        "toolchain": toolchain_snapshot(),
        "host": host_snapshot(),
        "environment": environment_snapshot(),
        "input_paths": inputs,
        "result_class": "未运行",
        "accepted": False,
        "limitations": ["Snapshot mode does not compile or run Mimi; command metrics remain unmeasured."],
    }
    data["content_hash"] = digest_json({k: v for k, v in data.items() if k != "content_hash"})
    json_write_exclusive(evidence / "snapshot.json", data)
    text_write_exclusive(evidence / "snapshot.json.sha256", (sha256_file(evidence / "snapshot.json") + "\n").encode())
    print(json.dumps({"evidence_dir": str(evidence), "source_identity": source["source_identity"], "result_class": "未运行"}, ensure_ascii=False))
    return 0


def collect_mode(args: argparse.Namespace) -> int:
    root = Path(args.root).resolve()
    if args.evidence_dir is None:
        raise ValueError("collect requires --evidence-dir")
    evidence = Path(args.evidence_dir).resolve()
    if (evidence / "manifest.json").exists():
        raise RuntimeError(f"evidence directory already has a manifest: {evidence}")
    evidence.mkdir(parents=True, exist_ok=True)
    (evidence / "logs").mkdir(exist_ok=True)
    (evidence / "records").mkdir(exist_ok=True)
    (evidence / "artifacts").mkdir(exist_ok=True)
    fixture_path = Path(args.fixture).resolve() if args.fixture else None
    specs, fixture_records, fixture_manifest = command_matrix_for_args(args, root, evidence)
    inputs = source_inputs_from_args(root, args.source, fixture_path)
    # Include explicit source paths from fixture manifests in the source hash.
    for row in fixture_records:
        if row["source"] not in inputs:
            inputs.append(row["source"])
    source = source_snapshot(root, inputs)
    toolchain = toolchain_snapshot()
    host = host_snapshot()
    source_identity = source["source_identity"]
    records: list[dict[str, Any]] = []
    # Keep a sequential runner by design.  B-08 records parallelism=1 rather
    # than accidentally contending with another agent's Cargo invocation.
    for spec in specs:
        repetitions = max(1, int(args.repeat if args.repeat is not None else spec.repetitions))
        warmups = max(0, int(args.warmup))
        selected_source = spec.source_path or next((item for item in inputs if item.endswith(".mimi") and Path(item).stem == spec.fixture), "")
        baseline_for_run = {"selected_source": selected_source, "inputs": inputs}
        for iteration in range(warmups):
            if args.no_execute:
                continue
            records.append(run_one(spec, root, evidence, source_identity, iteration, True, baseline_for_run))
        for iteration in range(repetitions):
            if args.no_execute:
                records.append({
                    "schema_version": SCHEMA_VERSION,
                    "evidence_kind": EVIDENCE_KIND,
                    "fixture_id": spec.fixture,
                    "command_id": spec.command_id,
                    "source_path": spec.source_path,
                    "input_kind": Path(spec.source_path).suffix.lstrip(".") if spec.source_path else "unknown",
                    "iteration": iteration,
                    "warmup": False,
                    "phase": spec.phase,
                    "command": list(spec.argv),
                    "route": {"intent": spec.route, "observed": None, "status": "unknown"},
                    "backend": {"intent": spec.backend, "observed": None, "status": "unknown"},
                    "verifier": {"intent": spec.verifier, "observed": None, "status": "unknown"},
                    "emitter_eligibility": {"status": "unknown", "eligible": None, "reason": "collection requested --no-execute"},
                    "source_identity": source_identity,
                    "result_class": "未运行",
                    "accepted": False,
                    "memory": {"peak_rss": {"value": None, "unit": "KiB", "status": "unknown"}},
                    "allocation": {"status": "unknown", "reason": "collection requested --no-execute"},
                    "copy_events": {"status": "unknown", "reason": "collection requested --no-execute"},
                    "limitations": ["Command intentionally not run; all execution metrics are unknown."],
                })
            else:
                records.append(run_one(spec, root, evidence, source_identity, iteration, False, baseline_for_run))
    aggregate_data = aggregate(records)
    census = static_census(root, [Path(row["source"]) for row in fixture_records])
    census["executable"] = executable_census(records)
    route_counts: dict[str, int] = {}
    backend_counts: dict[str, int] = {}
    verifier_counts: dict[str, int] = {}
    for row in records:
        for target, key in ((route_counts, row.get("route", {}).get("observed")), (backend_counts, row.get("backend", {}).get("observed")), (verifier_counts, row.get("verifier", {}).get("observed"))):
            if key:
                target[str(key)] = target.get(str(key), 0) + 1
    benchmark = {
        "warmup": int(args.warmup),
        "repetitions": int(args.repeat if args.repeat is not None else 1),
        "parallelism": 1,
        "parallelism_status": "fixed-sequential",
        "aggregate": aggregate_data,
        "route_observed_counts": route_counts,
        "backend_observed_counts": backend_counts,
        "verifier_observed_counts": verifier_counts,
        "allocation": {"status": "unknown", "reason": "No allocator event counter is inferred unless an explicit allocation tool is configured."},
        "copy_events": {"status": "unknown", "reason": "No aggregate copy/move counter is inferred unless explicit instrumentation is configured."},
        "thresholds": args.thresholds or {"status": "unknown", "reason": "B-04 thresholds must be frozen from independent product/oracle data."},
        "statistical_power": {"status": "unknown", "reason": "No control/effect-size model is inferred; pre-register G6 power separately."},
        "capacity": {
            "workers_available": os.cpu_count(),
            "workers_used": 1,
            "buffer": "unknown",
            "storage_bytes_free_at_start": shutil.disk_usage(evidence).free,
            "measurement_unit": "completed command samples",
        },
    }
    artifacts: list[dict[str, Any]] = []
    for row in records:
        artifact = row.get("artifact")
        if isinstance(artifact, dict):
            artifacts.append(artifact)
    for value in getattr(args, "artifact", []) or []:
        path = Path(value)
        if not path.is_absolute():
            path = root / path
        artifacts.append({**artifact_record(path, root), "kind": "explicit-extra-artifact", "required": True})
    non_warmup_records = [row for row in records if not row.get("warmup")]
    all_accepted = bool(non_warmup_records) and all(row.get("accepted") for row in non_warmup_records)
    all_not_run = bool(non_warmup_records) and all(row.get("result_class") == "未运行" for row in non_warmup_records)
    manifest: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": EVIDENCE_KIND,
        "created_at_utc": datetime.now(timezone.utc).isoformat(),
        "task_ids": ["B-01", "B-04", "B-08"],
        "root": str(root),
        "evidence_dir": str(evidence),
        "accepted": all_accepted,
        "result_class": "已通过" if all_accepted else ("未运行" if all_not_run or not records else "未解释失败"),
        "source": source,
        "toolchain": toolchain,
        "host": host,
        "environment": environment_snapshot(),
        "fixture_manifest": fixture_manifest,
        "fixtures": fixture_records,
        "command_count": len(specs),
        "command_matrix": [spec_record(spec) for spec in specs],
        "command_matrix_hash": digest_json([spec_record(spec) for spec in specs]),
        "records": records,
        "benchmark": benchmark,
        "census": census,
        "artifacts": artifacts,
        "raw_log_directory": relative(evidence / "logs", evidence),
        "record_directory": relative(evidence / "records", evidence),
        "limitations": [
            "A successful command demonstrates only the declared fixture/command, not the complete language target.",
            "Source identity includes tracked files and explicit fixture inputs; external toolchain identity is recorded by version/path/hash probes.",
            "Route/backend/verifier observed values are best-effort receipt/log observations; each command retains its declared intent.",
            "Peak RSS is a memory observation. Allocation/copy counts remain unknown without explicit instrumentation.",
            "The static owner census is not a reachability proof; executable route evidence and raw logs must be reviewed separately.",
        ],
    }
    manifest["content_hash"] = digest_json({k: v for k, v in manifest.items() if k != "content_hash"})
    json_write_exclusive(evidence / "benchmark.json", benchmark)
    json_write_exclusive(evidence / "census.json", census)
    json_write_exclusive(evidence / "manifest.json", manifest)
    text_write_exclusive(evidence / "manifest.json.sha256", (sha256_file(evidence / "manifest.json") + "\n").encode())
    print(json.dumps({
        "evidence_dir": str(evidence),
        "source_identity": source_identity,
        "commands": len(specs),
        "records": len(records),
        "result_class": manifest["result_class"],
        "accepted": manifest["accepted"],
    }, ensure_ascii=False))
    return 0 if manifest["accepted"] else (0 if args.no_execute else 2)


def verify_mode(args: argparse.Namespace) -> int:
    path = Path(args.manifest).resolve()
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != SCHEMA_VERSION or data.get("evidence_kind") != EVIDENCE_KIND:
        raise ValueError("unsupported baseline benchmark manifest schema")
    digest = data.get("content_hash")
    actual = digest_json({k: v for k, v in data.items() if k != "content_hash"})
    if digest != actual:
        raise ValueError("manifest content hash does not match")
    digest_path = path.with_name(path.name + ".sha256")
    if digest_path.is_file() and digest_path.read_text().strip() != sha256_file(path):
        raise ValueError("manifest detached sha256 does not match")
    changed: list[str] = []
    for row in data.get("records", []):
        logs = row.get("raw_logs", {})
        for stream in ("stdout", "stderr"):
            item = logs.get(stream, {})
            if not item:
                continue
            candidate = path.parent / item.get("path", "")
            if not candidate.is_file() or sha256_file(candidate) != item.get("sha256"):
                changed.append(str(candidate))
        artifact = row.get("artifact")
        if isinstance(artifact, dict) and artifact.get("sha256"):
            candidate = Path(artifact.get("absolute_path", ""))
            if not candidate.is_file() or sha256_file(candidate) != artifact.get("sha256"):
                changed.append(str(candidate))
    for artifact in data.get("artifacts", []):
        if not isinstance(artifact, dict) or not artifact.get("sha256"):
            continue
        candidate = Path(artifact.get("absolute_path", ""))
        if not candidate.is_file() or sha256_file(candidate) != artifact.get("sha256"):
            changed.append(str(candidate))
    if changed:
        raise ValueError("raw logs changed: " + ", ".join(changed))
    report = {"manifest": str(path), "content_hash_ok": True, "raw_logs_ok": True, "accepted": bool(data.get("accepted")), "records": len(data.get("records", []))}
    print(json.dumps(report, ensure_ascii=False))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--evidence-dir", type=Path, help="new evidence directory; existing manifests are never overwritten")
    sub = parser.add_subparsers(dest="mode", required=True)
    snapshot = sub.add_parser("snapshot", help="capture source/toolchain/host identity without running commands")
    snapshot.add_argument("--source", action="append", default=[], help="explicit source or fixture input (repeatable)")
    collect = sub.add_parser("collect", help="run a fixed command matrix and retain every result")
    collect.add_argument("--mimi", default="target/debug/mimi", help="mimi executable or PATH name")
    collect.add_argument("--source", action="append", default=[], help="source fixture path (repeatable)")
    collect.add_argument("--fixture", type=Path, help="JSON command matrix; --source filters fixture ids or source paths")
    collect.add_argument("--repeat", type=int, default=3, help="non-warmup repetitions per command")
    collect.add_argument("--warmup", type=int, default=1, help="warmups per command")
    collect.add_argument("--no-execute", action="store_true", help="write explicit 未运行 records for static planning")
    collect.add_argument("--thresholds", type=json.loads, help="JSON object of pre-registered thresholds; omitted values remain unknown")
    collect.add_argument("--artifact", "--artifact-file", action="append", default=[], help="extra runtime/native/FFI artifact to hash (repeatable)")
    verify = sub.add_parser("verify", help="verify manifest/log hashes without rerunning commands")
    verify.add_argument("manifest", type=Path)
    schema = sub.add_parser("schema", help="print the optional fixture manifest shape")
    schema.set_defaults(print_schema=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if getattr(args, "print_schema", False):
        print(json.dumps(fixture_schema(), ensure_ascii=False, indent=2))
        return 0
    args.root = args.root.resolve()
    if args.mode == "snapshot":
        return snapshot_mode(args)
    if args.mode == "collect":
        if args.repeat < 0 or args.warmup < 0:
            parser.error("--repeat and --warmup must be non-negative")
        return collect_mode(args)
    if args.mode == "verify":
        return verify_mode(args)
    raise AssertionError(args.mode)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        print(f"baseline evidence failed: {error}", file=sys.stderr)
        raise SystemExit(2)
