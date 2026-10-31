#!/usr/bin/env python3
"""Create and validate source-bound Cargo build receipts for Actor census runs.

The snapshot subcommand uses the same source enumerator as
``full_route_census.py``.  Receipts retain the raw command artifacts needed to
recompute Cargo argv, environment identity, the Cargo-reported Mimi binary,
and the exact source snapshot observed around the build.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any, Mapping, Sequence


RECEIPT_SCHEMA = "actor-census-build-receipt-v2"
ENVIRONMENT_SCHEMA = "actor-census-cargo-environment-v1"
CARGO_CONFIG_SCHEMA = "actor-census-cargo-config-snapshot-v1"
CARGO_IDENTITY_SCHEMA = "actor-census-cargo-identity-v1"
SENSITIVE_ENV_RE = re.compile(
    r"(?i)(token|password|secret|credential|authorization|private.?key|api.?key|access.?key)"
)
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _read_regular_file_nofollow(path: Path) -> bytes:
    """Read a regular file from one descriptor without following its final component."""
    nofollow = getattr(os, "O_NOFOLLOW", None)
    if nofollow is None:
        raise ValueError("O_NOFOLLOW is required for evidence-file reads")
    nonblock = getattr(os, "O_NONBLOCK", None)
    if nonblock is None:
        raise ValueError("O_NONBLOCK is required for evidence-file reads")
    # Opening a FIFO read-only can block before fstat gets a chance to reject it.
    # O_NONBLOCK has no effect on regular-file reads, but makes a final-component
    # FIFO swap return immediately so the descriptor can be rejected by fstat.
    flags = os.O_RDONLY | nofollow | nonblock | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(path, flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"file is not a regular file: {path}")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def sha256_file(path: Path) -> str:
    return sha256_bytes(_read_regular_file_nofollow(path))


def _write_json_exclusive(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, sort_keys=True, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def _read_json(path: Path) -> dict[str, Any]:
    value = json.loads(_read_regular_file_nofollow(path).decode("utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object in {path}")
    return value


def read_receipt(path: Path) -> tuple[dict[str, Any], str]:
    """Parse and hash one receipt byte sequence obtained from a single safe open."""
    raw = _read_regular_file_nofollow(path)
    value = json.loads(raw.decode("utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object in {path}")
    return value, sha256_bytes(raw)


def receipt_file_sha256(path: Path) -> str:
    """Re-open a receipt safely and return the digest for finalization checks."""
    return sha256_bytes(_read_regular_file_nofollow(path))


def _environment_digest(entries: Sequence[Mapping[str, Any]]) -> str:
    digest = hashlib.sha256()
    for entry in sorted(entries, key=lambda item: str(item.get("name", ""))):
        name = str(entry.get("name", ""))
        value_hash = str(entry.get("value_sha256", ""))
        digest.update(name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(value_hash.encode("ascii"))
        digest.update(b"\0\n")
    return digest.hexdigest()


def environment_document(environment: Mapping[str, str]) -> dict[str, Any]:
    entries: list[dict[str, Any]] = []
    for name, value in sorted(environment.items()):
        value_hash = sha256_bytes(os.fsencode(value))
        sensitive = SENSITIVE_ENV_RE.search(name) is not None
        entry: dict[str, Any] = {
            "name": name,
            "value_sha256": value_hash,
            "redacted": sensitive,
        }
        if not sensitive:
            entry["value"] = value
        entries.append(entry)
    return {
        "schema_version": ENVIRONMENT_SCHEMA,
        "working_directory": str(Path.cwd().resolve()),
        "entries": entries,
        "environment_sha256": _environment_digest(entries),
        "redaction_rule": "environment names matching token/password/secret/credential/auth/key are hash-only",
    }


def _environment_values(document: Mapping[str, Any]) -> dict[str, str]:
    entries = document.get("entries")
    values: dict[str, str] = {}
    if not isinstance(entries, list):
        return values
    for entry in entries:
        if (
            isinstance(entry, dict)
            and isinstance(entry.get("name"), str)
            and entry.get("redacted") is not True
            and isinstance(entry.get("value"), str)
        ):
            values[entry["name"]] = entry["value"]
    return values


def _rustup_output(root: Path, *arguments: str) -> str:
    try:
        result = subprocess.run(
            ["rustup", *arguments],
            cwd=root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
    except OSError as error:
        raise ValueError(f"could not run rustup {' '.join(arguments)}: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise ValueError(f"rustup {' '.join(arguments)} failed: {detail}")
    return result.stdout.strip()


def resolve_selected_toolchain_cargo(root: Path) -> Path:
    """Return rustup's canonical Cargo binary for the active toolchain at root."""
    output = _rustup_output(root.resolve(strict=True), "which", "cargo")
    if not output or "\n" in output:
        raise ValueError("rustup which cargo did not return exactly one executable path")
    path = Path(output)
    if not path.is_absolute():
        raise ValueError("rustup which cargo returned a non-absolute path")
    try:
        canonical = path.resolve(strict=True)
        metadata = canonical.stat()
    except OSError as error:
        raise ValueError(f"selected rustup Cargo executable is unavailable: {error}") from error
    if str(canonical) != output or canonical.name != "cargo":
        raise ValueError("selected rustup Cargo path is not canonical or is not named cargo")
    if not stat.S_ISREG(metadata.st_mode) or not os.access(canonical, os.X_OK):
        raise ValueError("selected rustup Cargo path is not a regular executable")
    return canonical


def cargo_identity_document(root: Path, cargo_path: Path) -> dict[str, Any]:
    """Bind Cargo to rustup's selected binary, its version, and stable bytes."""
    root = root.resolve(strict=True)
    canonical = resolve_selected_toolchain_cargo(root)
    try:
        supplied = cargo_path.resolve(strict=True)
    except OSError as error:
        raise ValueError(f"recorded Cargo executable is unavailable: {error}") from error
    if supplied != canonical or str(cargo_path) != str(canonical):
        raise ValueError("Cargo argv[0] is not the canonical rustup-selected Cargo executable")
    selected_toolchain = _rustup_output(root, "show", "active-toolchain")
    if not selected_toolchain or "\n" in selected_toolchain:
        raise ValueError("rustup did not report one active toolchain")
    before = sha256_file(canonical)
    try:
        result = subprocess.run(
            [str(canonical), "--version"],
            cwd=root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise ValueError(f"could not query selected Cargo version: {error}") from error
    after = sha256_file(canonical)
    if result.returncode != 0 or result.stderr:
        raise ValueError("selected Cargo --version did not exit cleanly")
    try:
        version = result.stdout.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise ValueError("selected Cargo version output is not UTF-8") from error
    if not re.fullmatch(r"cargo\s+\d+\.\d+\.\d+(?:\s+.*)?", version):
        raise ValueError("selected rustup executable did not report a Cargo version")
    if before != after:
        raise ValueError("selected Cargo executable changed while its version was queried")
    return {
        "schema_version": CARGO_IDENTITY_SCHEMA,
        "path": str(canonical),
        "selected_toolchain": selected_toolchain,
        "version": version,
        "sha256_before": before,
        "sha256_after": after,
    }


def _cargo_config_candidate_paths(root: Path, environment: Mapping[str, str]) -> tuple[str, list[Path]]:
    cargo_home_value = environment.get("CARGO_HOME")
    if cargo_home_value == "":
        raise ValueError("effective CARGO_HOME is set to an empty path")
    if cargo_home_value is None:
        home = environment.get("HOME")
        cargo_home = (Path(home) if home else Path.home()) / ".cargo"
    else:
        cargo_home = Path(cargo_home_value)
        if not cargo_home.is_absolute():
            cargo_home = root / cargo_home
    cargo_home = cargo_home.resolve(strict=False)

    candidates: list[Path] = []
    for directory in (root, *root.parents):
        cargo_dir = directory / ".cargo"
        candidates.extend(cargo_dir / name for name in ("config.toml", "config"))
    candidates.extend(cargo_home / name for name in ("config.toml", "config"))
    unique = sorted({candidate.resolve(strict=False) for candidate in candidates}, key=str)
    return str(cargo_home), unique


def _load_cargo_config(raw: bytes, path: Path) -> dict[str, Any]:
    try:
        import tomllib
    except ImportError:
        try:
            import tomli as tomllib  # type: ignore[no-redef,import-not-found]
        except ImportError as error:
            raise ValueError("tomllib or tomli is required to inspect Cargo config") from error
    try:
        value = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeError, ValueError) as error:
        raise ValueError(f"Cargo config cannot be parsed: {path}") from error
    if not isinstance(value, dict):
        raise ValueError(f"Cargo config is not a TOML table: {path}")
    return value


def _safe_config_setting(key: str, value: Any) -> Any:
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    if SENSITIVE_ENV_RE.search(key) or SENSITIVE_ENV_RE.search(encoded):
        return {"redacted": True, "value_sha256": sha256_bytes(encoded.encode("utf-8"))}
    return value


def _cargo_compiler_settings(config: dict[str, Any]) -> dict[str, Any]:
    settings: dict[str, Any] = {}
    build = config.get("build", {})
    if isinstance(build, dict):
        selected = {
            key: _safe_config_setting(key, build[key])
            for key in (
                "target", "rustc", "rustc-wrapper", "rustc-workspace-wrapper",
                "rustflags", "rustc-linker", "target-dir", "incremental", "jobs",
            )
            if key in build
        }
        if selected:
            settings["build"] = selected
    target = config.get("target", {})
    if isinstance(target, dict):
        target_settings: dict[str, Any] = {}
        for target_name, target_value in sorted(target.items()):
            if not isinstance(target_value, dict):
                continue
            selected = {
                key: _safe_config_setting(key, target_value[key])
                for key in ("linker", "runner", "rustflags")
                if key in target_value
            }
            if selected:
                target_settings[str(target_name)] = selected
        if target_settings:
            settings["target"] = target_settings
    env_table = config.get("env", {})
    relevant_env = {
        "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER", "CC", "CXX", "AR", "LD",
        "LLVM_SYS_181_PREFIX", "LIBCLANG_PATH",
    }
    if isinstance(env_table, dict):
        selected_env = {
            str(key): _safe_config_setting(str(key), env_table[key])
            for key in sorted(env_table)
            if str(key) in relevant_env
        }
        if selected_env:
            settings["env"] = selected_env
    return settings


def cargo_config_snapshot(root: Path, environment: Mapping[str, str]) -> dict[str, Any]:
    """Hash every repo/ancestor/CARGO_HOME Cargo config candidate, including absences."""
    root = root.resolve(strict=True)
    cargo_home, paths = _cargo_config_candidate_paths(root, environment)
    entries: list[dict[str, Any]] = []
    for path in paths:
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            entries.append({"path": str(path), "present": False, "sha256": None, "bytes": None})
            continue
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"Cargo config is not a regular non-symlink file: {path}")
        raw = _read_regular_file_nofollow(path)
        settings = _cargo_compiler_settings(_load_cargo_config(raw, path))
        entries.append({
            "path": str(path),
            "present": True,
            "sha256": sha256_bytes(raw),
            "bytes": len(raw),
            "compiler_settings": settings,
        })
    digest_payload = json.dumps(entries, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return {
        "schema_version": CARGO_CONFIG_SCHEMA,
        "root": str(root),
        "cargo_home": cargo_home,
        "files": entries,
        "manifest_sha256": sha256_bytes(digest_payload),
    }


def capture_cargo_identity(root: Path, cargo_path: Path, output: Path) -> dict[str, Any]:
    value = cargo_identity_document(root, cargo_path)
    _write_json_exclusive(output, value)
    return value


def capture_cargo_config(root: Path, output: Path) -> dict[str, Any]:
    value = cargo_config_snapshot(root, os.environ)
    _write_json_exclusive(output, value)
    return value


def _validate_environment(value: Any) -> tuple[list[str], dict[str, dict[str, Any]]]:
    issues: list[str] = []
    document = value if isinstance(value, dict) else {}
    if document.get("schema_version") != ENVIRONMENT_SCHEMA:
        issues.append("Cargo environment record has an unsupported schema")
    entries_value = document.get("entries")
    if not isinstance(entries_value, list):
        issues.append("Cargo environment entries are absent")
        entries_value = []
    by_name: dict[str, dict[str, Any]] = {}
    for entry_value in entries_value:
        if not isinstance(entry_value, dict):
            issues.append("Cargo environment contains a non-object entry")
            continue
        name = entry_value.get("name")
        value_hash = entry_value.get("value_sha256")
        if not isinstance(name, str) or not name or name in by_name:
            issues.append("Cargo environment contains an invalid or duplicate name")
            continue
        if not isinstance(value_hash, str) or not SHA256_RE.fullmatch(value_hash):
            issues.append(f"Cargo environment has an invalid value hash for {name}")
            continue
        if entry_value.get("redacted") is True:
            if "value" in entry_value:
                issues.append(f"sensitive Cargo environment value was not redacted: {name}")
        else:
            actual_hash = sha256_bytes(os.fsencode(str(entry_value.get("value", ""))))
            if actual_hash != value_hash:
                issues.append(f"Cargo environment value hash differs for {name}")
        by_name[name] = dict(entry_value)
    expected_digest = _environment_digest(list(by_name.values()))
    if document.get("environment_sha256") != expected_digest:
        issues.append("Cargo environment fingerprint does not match its entries")
    return issues, by_name


def _decode_nul_records(path: Path) -> list[str]:
    raw = _read_regular_file_nofollow(path)
    return _decode_nul_records_bytes(raw, path)


def _decode_nul_records_bytes(raw: bytes, label: Path | str) -> list[str]:
    if not raw or not raw.endswith(b"\0"):
        raise ValueError(f"NUL record file is empty or unterminated: {label}")
    try:
        values = [os.fsdecode(item) for item in raw[:-1].split(b"\0")]
    except UnicodeError as error:
        raise ValueError(f"could not decode NUL records in {label}: {error}") from error
    if any(not item for item in values):
        raise ValueError(f"NUL record file contains an empty field: {label}")
    return values


def _decode_sha256sum_filename(raw: bytes) -> bytes:
    if not raw.startswith(b"\\"):
        return raw
    raw = raw[1:]
    output = bytearray()
    index = 0
    while index < len(raw):
        byte = raw[index]
        if byte == 0x5C and index + 1 < len(raw):
            following = raw[index + 1]
            if following == ord("n"):
                output.append(0x0A)
                index += 2
                continue
            if following == ord("r"):
                output.append(0x0D)
                index += 2
                continue
            if following == ord("\\"):
                output.append(0x5C)
                index += 2
                continue
        output.append(byte)
        index += 1
    return bytes(output)


def manifest_from_snapshot_files(inventory_path: Path, hashes_path: Path) -> dict[str, Any]:
    paths = _decode_nul_records(inventory_path)
    hash_lines = _read_regular_file_nofollow(hashes_path).splitlines()
    if len(paths) != len(hash_lines):
        raise ValueError(
            f"source inventory/hash row count differs: {len(paths)} paths, {len(hash_lines)} rows"
        )
    files: dict[str, dict[str, Any]] = {}
    for path, line in zip(paths, hash_lines, strict=True):
        escaped = line.startswith(b"\\")
        record = line[1:] if escaped else line
        if len(record) < 66 or record[64:66] != b"  ":
            raise ValueError(f"invalid sha256sum row for {path}")
        try:
            digest = record[:64].decode("ascii")
        except UnicodeDecodeError as error:
            raise ValueError(f"invalid SHA-256 text for {path}") from error
        if not SHA256_RE.fullmatch(digest):
            raise ValueError(f"invalid SHA-256 value for {path}")
        recorded_path = _decode_sha256sum_filename(record[66:])
        if recorded_path != os.fsencode(path):
            raise ValueError(f"sha256sum path order differs from source inventory for {path}")
        if path in files:
            raise ValueError(f"duplicate source path in snapshot inventory: {path}")
        files[path] = {"sha256": digest}
    if list(files) != sorted(files):
        raise ValueError("source snapshot inventory is not in canonical path order")
    return {"files": files, "manifest_sha256": source_manifest_digest(files)}


def reconcile_source_manifests(
    outer_before: Mapping[str, Mapping[str, Any]],
    outer_after: Mapping[str, Mapping[str, Any]],
    census_files: Mapping[str, Mapping[str, Any]],
    required_paths: Sequence[str],
) -> tuple[dict[str, Any], list[str]]:
    """Describe the curated outer inventory and full collector-snapshot delta."""
    issues: list[str] = []
    before = {str(path): dict(record) for path, record in outer_before.items()}
    after = {str(path): dict(record) for path, record in outer_after.items()}
    census = {str(path): dict(record) for path, record in census_files.items()}
    if before != after:
        issues.append("runner curated outer source path/hash manifest changed during replay")
    shared = sorted(set(before) & set(census))
    shared_hash_mismatches = [
        path for path in shared
        if before[path].get("sha256") != census[path].get("sha256")
    ]
    if shared_hash_mismatches:
        issues.append("runner outer and collector source hashes differ on shared paths")
    missing_required_outer = sorted(set(required_paths) - set(before))
    missing_required_collector = sorted(set(required_paths) - set(census))
    if missing_required_outer:
        issues.append("runner outer inventory omits required build/runner sources")
    if missing_required_collector:
        issues.append("collector source snapshot omits required build/runner sources")
    for path in sorted(set(required_paths) & set(before) & set(census)):
        if before[path].get("sha256") != census[path].get("sha256"):
            issues.append(f"required build/runner source differs between inventories: {path}")

    outer_only: list[dict[str, Any]] = []
    for path in sorted(set(before) - set(census)):
        suffix = Path(path).suffix.lower()
        compiler_like = suffix in {".mimi", ".rs", ".c", ".h", ".cc", ".cpp"} or Path(path).name in {"build.rs", "build.sh"}
        reason = (
            "collector_snapshot_missing_compiler_or_build_candidate"
            if compiler_like
            else "runner_curated_tree_file_not_selected_by_collector_compiler_snapshot"
        )
        if compiler_like:
            issues.append(f"collector snapshot omits outer compiler/build input: {path}")
        outer_only.append({"path": path, "sha256": before[path].get("sha256"), "reason": reason})

    collector_only: list[dict[str, Any]] = []
    for path in sorted(set(census) - set(before)):
        suffix = Path(path).suffix.lower()
        if suffix == ".mimi":
            reason = "active_mimi_candidate_outside_curated_src_tests_std_roots"
        elif suffix in {".rs", ".c", ".h", ".cc", ".cpp"} or Path(path).name in {"build.rs", "build.sh"}:
            reason = "collector_compiler_or_build_input_outside_curated_src_tests_std_roots"
        elif path.startswith(".cargo/") or path in {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"}:
            reason = "collector_explicit_cargo_or_toolchain_input_outside_curated_roots"
        else:
            reason = "collector_explicit_or_enumerated_input_outside_curated_roots"
        collector_only.append({"path": path, "sha256": census[path].get("sha256"), "reason": reason})

    document = {
        "schema_version": "actor-census-source-scope-reconciliation-v1",
        "outer_inventory_kind": "runner-curated-src-tests-stdlib-required-inputs",
        "collector_inventory_kind": "full_route_census.source_snapshot.files",
        "outer_before_count": len(before),
        "outer_after_count": len(after),
        "collector_count": len(census),
        "shared_count": len(shared),
        "outer_only": outer_only,
        "collector_only": collector_only,
        "shared_hash_mismatches": shared_hash_mismatches,
        "missing_required_outer": missing_required_outer,
        "missing_required_collector": missing_required_collector,
        "required_paths": list(required_paths),
        "equivalence_claim": "none; curated outer and full collector source scopes are intentionally recorded separately",
        "issues": sorted(set(issues)),
        "status": "reconciled" if not issues else "invalid",
    }
    return document, sorted(set(issues))


def source_manifest_digest(files: Mapping[str, Mapping[str, Any]]) -> str:
    digest = hashlib.sha256()
    for name, record in sorted(files.items()):
        value_hash = record.get("sha256")
        if value_hash is None:
            value_hash = "missing"
        if not isinstance(value_hash, str) or (value_hash != "missing" and not SHA256_RE.fullmatch(value_hash)):
            raise ValueError(f"source manifest has a missing or invalid hash: {name}")
        digest.update(name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(value_hash.encode("ascii"))
        digest.update(b"\0\n")
    return digest.hexdigest()


def source_attestation_digest(source_digest: str | None, cargo_config: Mapping[str, Any]) -> str:
    payload = json.dumps(
        {"source_manifest_sha256": source_digest, "cargo_config": dict(cargo_config)},
        sort_keys=True,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return sha256_bytes(payload)


def _snapshot_id(files: Mapping[str, Mapping[str, Any]]) -> str:
    digest = hashlib.sha256()
    for name, record in sorted(files.items()):
        digest.update(name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(str(record.get("sha256") or "missing").encode("ascii"))
        digest.update(b"\0")
        digest.update(str(record.get("kind")).encode("utf-8"))
        digest.update(b"\n")
    return "sha256:" + digest.hexdigest()


def validate_source_snapshot(value: Any, *, label: str) -> tuple[list[str], dict[str, dict[str, Any]]]:
    issues: list[str] = []
    snapshot = value if isinstance(value, dict) else {}
    files_value = snapshot.get("files")
    if not isinstance(files_value, dict):
        return [f"{label} source snapshot has no files object"], {}
    files: dict[str, dict[str, Any]] = {}
    for path, record_value in files_value.items():
        if not isinstance(path, str) or not path or Path(path).is_absolute() or ".." in Path(path).parts:
            issues.append(f"{label} has an unsafe source path: {path!r}")
            continue
        if not isinstance(record_value, dict):
            issues.append(f"{label} source record is not an object: {path}")
            continue
        value_hash = record_value.get("sha256")
        kind = record_value.get("kind")
        if value_hash is None:
            if kind not in {"mimi_missing_tracked_input", "mimi_blocked_by_symlinked_parent"}:
                issues.append(f"{label} source record has no current SHA-256: {path}")
        elif not isinstance(value_hash, str) or not SHA256_RE.fullmatch(value_hash):
            issues.append(f"{label} source record has an invalid SHA-256: {path}")
            continue
        if kind not in {
            "mimi", "mimi_missing_tracked_input", "mimi_blocked_by_symlinked_parent",
            "mimi_symlink_not_followed", "mimi_unknown_presence", "compiler_or_build_input",
        }:
            issues.append(f"{label} source record is not a present active input: {path}")
        files[path] = dict(record_value)
    if len(files) != len(files_value):
        issues.append(f"{label} contains duplicate or invalid source records")
    if snapshot.get("file_count") != len(files):
        issues.append(f"{label} file_count differs from its source records")
    if snapshot.get("snapshot_id") != _snapshot_id(files):
        issues.append(f"{label} snapshot_id does not recompute from its path/hash/kind records")
    return issues, files


def _read_exit(path: Path) -> int:
    return _read_exit_bytes(_read_regular_file_nofollow(path), path)


def _read_exit_bytes(raw: bytes, label: Path | str) -> int:
    text = raw.decode("utf-8").strip()
    match = re.fullmatch(r"exit_code=(\d+)", text)
    if match is None:
        raise ValueError(f"invalid Cargo exit record in {label}: {text or '<empty>'}")
    return int(match.group(1))


def _parse_mimi_artifact(stdout: bytes, target_dir: Path, host_target: str) -> tuple[Path, str]:
    events: list[dict[str, Any]] = []
    try:
        text = stdout.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValueError(f"Cargo JSON output is not UTF-8: {error}") from error
    for line in text.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict) or event.get("reason") != "compiler-artifact":
            continue
        target = event.get("target")
        if (
            isinstance(target, dict)
            and target.get("name") == "mimi"
            and isinstance(target.get("kind"), list)
            and "bin" in target["kind"]
        ):
            events.append(event)
    if len(events) != 1:
        raise ValueError(f"expected exactly one Cargo-reported Mimi binary artifact, observed {len(events)}")
    raw_path = events[0].get("executable")
    if not isinstance(raw_path, str) or not Path(raw_path).is_absolute():
        raise ValueError("the unique Cargo-reported Mimi artifact has no absolute executable path")
    try:
        target_root = target_dir.resolve(strict=True)
        executable = Path(raw_path).resolve(strict=True)
    except OSError as error:
        raise ValueError(f"Cargo-reported isolated target or Mimi executable is unavailable: {error}") from error
    try:
        relative = executable.relative_to(target_root)
    except ValueError as error:
        raise ValueError("Cargo-reported Mimi executable escaped the fresh isolated target") from error
    if not relative.parts or relative.parts[0] != host_target:
        raise ValueError("Cargo-reported Mimi executable is outside the forced host-target subtree")
    if str(executable) != raw_path:
        raise ValueError("Cargo-reported Mimi executable path is not canonical")
    metadata = executable.stat()
    if not stat.S_ISREG(metadata.st_mode) or not os.access(executable, os.X_OK):
        raise ValueError("Cargo-reported Mimi executable is not a regular executable file")
    return executable, sha256_file(executable)


def _relative_evidence_path(artifact_root: Path, path: Path) -> str:
    resolved_root = artifact_root.resolve(strict=True)
    resolved = path.resolve(strict=True)
    try:
        return resolved.relative_to(resolved_root).as_posix()
    except ValueError as error:
        raise ValueError(f"build receipt evidence escaped the artifact directory: {path}") from error


def _evidence_record(artifact_root: Path, path: Path) -> dict[str, Any]:
    relative = _relative_evidence_path(artifact_root, path)
    data = _read_regular_file_nofollow(path)
    return {"path": relative, "bytes": len(data), "sha256": sha256_bytes(data)}


def _safe_evidence_file(
    artifact_root: Path, record: Any, label: str
) -> tuple[Path | None, bytes | None, list[str]]:
    issues: list[str] = []
    if not isinstance(record, dict):
        return None, None, [f"receipt evidence record is absent: {label}"]
    relative_value = record.get("path")
    if not isinstance(relative_value, str):
        return None, None, [f"receipt evidence path is invalid: {label}"]
    relative = Path(relative_value)
    if relative.is_absolute() or ".." in relative.parts:
        return None, None, [f"receipt evidence path escapes its artifact directory: {label}"]
    path = artifact_root / relative
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(artifact_root.resolve(strict=True))
    except (OSError, ValueError):
        return None, None, [f"receipt evidence path resolves outside its artifact directory: {label}"]
    try:
        data = _read_regular_file_nofollow(path)
    except (OSError, ValueError) as error:
        return None, None, [f"receipt evidence file is absent, non-regular, or symlinked: {label}: {error}"]
    expected_hash = record.get("sha256")
    if not isinstance(expected_hash, str) or not SHA256_RE.fullmatch(expected_hash):
        issues.append(f"receipt evidence hash is invalid: {label}")
    elif sha256_bytes(data) != expected_hash:
        issues.append(f"receipt evidence file hash differs: {label}")
    if record.get("bytes") != len(data):
        issues.append(f"receipt evidence byte count differs: {label}")
    return path, data, issues


def _source_manifest_from_snapshot(snapshot_path: Path, *, label: str) -> tuple[list[str], dict[str, dict[str, Any]]]:
    try:
        value = _read_json(snapshot_path)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        return [f"{label} raw source snapshot is unavailable or invalid: {error}"], {}
    return validate_source_snapshot(value, label=label)


def _source_manifest_from_snapshot_bytes(
    raw: bytes, *, label: str
) -> tuple[list[str], dict[str, dict[str, Any]]]:
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError, ValueError) as error:
        return [f"{label} raw source snapshot is unavailable or invalid: {error}"], {}
    return validate_source_snapshot(value, label=label)


def _read_argv(path: Path) -> list[str]:
    values = _decode_nul_records(path)
    if len(values) < 2:
        raise ValueError("Cargo argv must include an executable and command")
    return values


def _read_argv_bytes(raw: bytes) -> list[str]:
    values = _decode_nul_records_bytes(raw, "Cargo argv")
    if len(values) < 2:
        raise ValueError("Cargo argv must include an executable and command")
    return values


def _json_object_bytes(raw: bytes, label: str) -> dict[str, Any]:
    value = json.loads(raw.decode("utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object in {label}")
    return value


def _write_receipt_paths(gate_dir: Path, executable: Path, executable_hash: str) -> None:
    with (gate_dir / "mimi.path").open("x", encoding="utf-8") as stream:
        stream.write(str(executable) + "\n")
    with (gate_dir / "mimi.sha256").open("x", encoding="ascii") as stream:
        stream.write(executable_hash + "\n")


def create_receipt(
    *, artifact_root: Path, gate_dir: Path, target_dir: Path, host_target: str,
    root: Path, receipt_path: Path,
) -> tuple[dict[str, Any], list[str]]:
    issues: list[str] = []
    artifact_root = artifact_root.resolve(strict=True)
    gate_dir = gate_dir.resolve(strict=True)
    target_dir = target_dir.resolve(strict=True)
    root = root.resolve(strict=True)
    evidence_paths = {
        "cargo_argv": gate_dir / "argv.nul",
        "cargo_environment": gate_dir / "cargo-environment.json",
        "cargo_identity_before": gate_dir / "cargo-identity.before.json",
        "cargo_config_before": gate_dir / "cargo-config.before.json",
        "cargo_identity_after": gate_dir / "cargo-identity.after.json",
        "cargo_config_after": gate_dir / "cargo-config.after.json",
        "environment_capture_argv": gate_dir / "environment-capture.argv.nul",
        "environment_capture_exit": gate_dir / "environment-capture.exit.txt",
        "environment_capture_stdout": gate_dir / "environment-capture.stdout.bin",
        "environment_capture_stderr": gate_dir / "environment-capture.stderr.bin",
        "source_before_capture_argv": gate_dir / "source-before-capture.argv.nul",
        "source_before_capture_exit": gate_dir / "source-before-capture.exit.txt",
        "source_before_capture_stdout": gate_dir / "source-before-capture.stdout.bin",
        "source_before_capture_stderr": gate_dir / "source-before-capture.stderr.bin",
        "source_after_capture_argv": gate_dir / "source-after-capture.argv.nul",
        "source_after_capture_exit": gate_dir / "source-after-capture.exit.txt",
        "source_after_capture_stdout": gate_dir / "source-after-capture.stdout.bin",
        "source_after_capture_stderr": gate_dir / "source-after-capture.stderr.bin",
        "cargo_identity_before_capture_argv": gate_dir / "cargo-identity-before-capture.argv.nul",
        "cargo_identity_before_capture_exit": gate_dir / "cargo-identity-before-capture.exit.txt",
        "cargo_identity_before_capture_stdout": gate_dir / "cargo-identity-before-capture.stdout.bin",
        "cargo_identity_before_capture_stderr": gate_dir / "cargo-identity-before-capture.stderr.bin",
        "cargo_config_before_capture_argv": gate_dir / "cargo-config-before-capture.argv.nul",
        "cargo_config_before_capture_exit": gate_dir / "cargo-config-before-capture.exit.txt",
        "cargo_config_before_capture_stdout": gate_dir / "cargo-config-before-capture.stdout.bin",
        "cargo_config_before_capture_stderr": gate_dir / "cargo-config-before-capture.stderr.bin",
        "cargo_identity_after_capture_argv": gate_dir / "cargo-identity-after-capture.argv.nul",
        "cargo_identity_after_capture_exit": gate_dir / "cargo-identity-after-capture.exit.txt",
        "cargo_identity_after_capture_stdout": gate_dir / "cargo-identity-after-capture.stdout.bin",
        "cargo_identity_after_capture_stderr": gate_dir / "cargo-identity-after-capture.stderr.bin",
        "cargo_config_after_capture_argv": gate_dir / "cargo-config-after-capture.argv.nul",
        "cargo_config_after_capture_exit": gate_dir / "cargo-config-after-capture.exit.txt",
        "cargo_config_after_capture_stdout": gate_dir / "cargo-config-after-capture.stdout.bin",
        "cargo_config_after_capture_stderr": gate_dir / "cargo-config-after-capture.stderr.bin",
        "cargo_exit": gate_dir / "exit.txt",
        "cargo_stdout": gate_dir / "stdout.bin",
        "cargo_stderr": gate_dir / "stderr.bin",
        "fresh_target_marker": gate_dir / "fresh-target.txt",
        "source_before_snapshot": gate_dir / "source.before.json",
        "source_after_snapshot": gate_dir / "source.after.json",
    }
    try:
        cargo_argv = _read_argv(evidence_paths["cargo_argv"])
    except (OSError, ValueError) as error:
        issues.append(f"Cargo argv is unavailable or invalid: {error}")
        cargo_argv = []
    try:
        cargo_environment = _read_json(evidence_paths["cargo_environment"])
        env_issues, env_by_name = _validate_environment(cargo_environment)
        issues.extend(env_issues)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        issues.append(f"Cargo environment is unavailable or invalid: {error}")
        cargo_environment = {}
        env_by_name = {}
    capture_specs = {
        "environment_capture": [
            str(Path(__file__).resolve()), "capture-environment", "--output",
            str(evidence_paths["cargo_environment"]),
        ],
        "source_before_capture": [
            str(Path(__file__).resolve()), "capture-source-snapshot", "--root", str(root),
            "--output", str(evidence_paths["source_before_snapshot"]),
        ],
        "source_after_capture": [
            str(Path(__file__).resolve()), "capture-source-snapshot", "--root", str(root),
            "--output", str(evidence_paths["source_after_snapshot"]),
        ],
        "cargo_identity_before_capture": [
            str(Path(__file__).resolve()), "capture-cargo-identity", "--root", str(root),
            "--cargo", cargo_argv[0] if cargo_argv else "",
            "--output", str(evidence_paths["cargo_identity_before"]),
        ],
        "cargo_config_before_capture": [
            str(Path(__file__).resolve()), "capture-cargo-config", "--root", str(root),
            "--output", str(evidence_paths["cargo_config_before"]),
        ],
        "cargo_identity_after_capture": [
            str(Path(__file__).resolve()), "capture-cargo-identity", "--root", str(root),
            "--cargo", cargo_argv[0] if cargo_argv else "",
            "--output", str(evidence_paths["cargo_identity_after"]),
        ],
        "cargo_config_after_capture": [
            str(Path(__file__).resolve()), "capture-cargo-config", "--root", str(root),
            "--output", str(evidence_paths["cargo_config_after"]),
        ],
    }
    for prefix, expected_suffix in capture_specs.items():
        evidence_prefix = prefix.replace("_", "-")
        try:
            argv = _read_argv(evidence_paths[f"{prefix}_argv"])
            capture_exit = _read_exit(evidence_paths[f"{prefix}_exit"])
            capture_stdout = _read_regular_file_nofollow(evidence_paths[f"{prefix}_stdout"])
            capture_stderr = _read_regular_file_nofollow(evidence_paths[f"{prefix}_stderr"])
            if argv[-len(expected_suffix):] != expected_suffix:
                issues.append(f"raw {evidence_prefix} argv does not invoke this helper for the stored input/output")
            if capture_exit != 0 or capture_stdout or capture_stderr:
                issues.append(f"raw {evidence_prefix} capture did not exit cleanly with empty stdout/stderr")
        except (KeyError, OSError, UnicodeError, ValueError) as error:
            issues.append(f"raw {evidence_prefix} command is unavailable or invalid: {error}")
    try:
        cargo_exit = _read_exit(evidence_paths["cargo_exit"])
    except (OSError, UnicodeError, ValueError) as error:
        issues.append(f"Cargo exit record is unavailable or invalid: {error}")
        cargo_exit = None
    try:
        stdout = _read_regular_file_nofollow(evidence_paths["cargo_stdout"])
        stderr = _read_regular_file_nofollow(evidence_paths["cargo_stderr"])
    except (OSError, ValueError) as error:
        issues.append(f"Cargo stdout/stderr are unavailable or unsafe: {error}")
        stdout = b""
        stderr = b""
    if cargo_exit != 0:
        issues.append(f"Cargo build did not exit successfully: {cargo_exit}")
    expected_cargo_args = [
        "build", "--bin", "mimi", "--locked", "--target", host_target,
        "--message-format=json-render-diagnostics",
    ]
    if cargo_argv[1:] != expected_cargo_args:
        issues.append("Cargo argv differs from the fixed isolated Mimi build command")
    if not cargo_argv:
        issues.append("Cargo argv is empty")
    elif not Path(cargo_argv[0]).is_absolute():
        issues.append("Cargo executable path in argv is not absolute")

    cargo_identity_before: dict[str, Any] = {}
    cargo_identity_after: dict[str, Any] = {}
    cargo_config_before: dict[str, Any] = {}
    cargo_config_after: dict[str, Any] = {}
    try:
        cargo_identity_before = _read_json(evidence_paths["cargo_identity_before"])
        cargo_identity_after = _read_json(evidence_paths["cargo_identity_after"])
        cargo_config_before = _read_json(evidence_paths["cargo_config_before"])
        cargo_config_after = _read_json(evidence_paths["cargo_config_after"])
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        issues.append(f"Cargo identity/configuration snapshots are unavailable or invalid: {error}")
    expected_identity = None
    if cargo_argv:
        try:
            expected_identity = cargo_identity_document(root, Path(cargo_argv[0]))
        except (OSError, ValueError) as error:
            issues.append(f"Cargo argv[0] is not the selected rustup Cargo executable: {error}")
    if cargo_identity_before.get("schema_version") != CARGO_IDENTITY_SCHEMA:
        issues.append("pre-build Cargo identity snapshot has an unsupported schema")
    if cargo_identity_after.get("schema_version") != CARGO_IDENTITY_SCHEMA:
        issues.append("post-build Cargo identity snapshot has an unsupported schema")
    if cargo_identity_before != cargo_identity_after:
        issues.append("selected Cargo path/version/hash changed during the build")
    if expected_identity is not None and cargo_identity_before != expected_identity:
        issues.append("raw Cargo identity differs from the selected rustup executable bytes/version")
    if cargo_argv and cargo_identity_before.get("path") != cargo_argv[0]:
        issues.append("Cargo receipt identity path differs from raw argv[0]")
    config_environment = _environment_values(cargo_environment)
    try:
        expected_config = cargo_config_snapshot(root, config_environment)
    except (OSError, ValueError) as error:
        expected_config = None
        issues.append(f"could not recompute effective Cargo config files: {error}")
    if cargo_config_before.get("schema_version") != CARGO_CONFIG_SCHEMA:
        issues.append("pre-build Cargo config snapshot has an unsupported schema")
    if cargo_config_after.get("schema_version") != CARGO_CONFIG_SCHEMA:
        issues.append("post-build Cargo config snapshot has an unsupported schema")
    if cargo_config_before != cargo_config_after:
        issues.append("effective Cargo config files changed during the build")
    if expected_config is not None and cargo_config_before != expected_config:
        issues.append("effective Cargo config files changed since the isolated build")
    target_env = env_by_name.get("CARGO_TARGET_DIR", {}).get("value")
    if target_env != str(target_dir):
        issues.append("effective Cargo CARGO_TARGET_DIR does not match the fresh isolated target")
    if env_by_name.get("CARGO_BUILD_JOBS", {}).get("value") != "1":
        issues.append("effective Cargo CARGO_BUILD_JOBS is not fixed at one")
    if env_by_name.get("CARGO_TERM_COLOR", {}).get("value") != "never":
        issues.append("effective Cargo CARGO_TERM_COLOR is not fixed to never")
    if cargo_environment.get("working_directory") != str(root):
        issues.append("effective Cargo working directory differs from the repository root")
    for forbidden in ("CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS"):
        if forbidden in env_by_name:
            issues.append(f"effective Cargo environment unexpectedly retains {forbidden}")

    try:
        target_marker = _read_regular_file_nofollow(evidence_paths["fresh_target_marker"]).decode("utf-8").splitlines()
        marker_values = dict(line.split("=", 1) for line in target_marker if "=" in line)
    except (OSError, UnicodeError, ValueError) as error:
        issues.append(f"fresh target marker is unavailable or invalid: {error}")
        marker_values = {}
    if marker_values.get("target_path") != str(target_dir):
        issues.append("fresh target marker path differs from the isolated target")
    if marker_values.get("directory_existed_before") != "false" or marker_values.get("created_exclusively") != "true":
        issues.append("Cargo target directory was not recorded as freshly and exclusively created")

    before_issues, before_files = _source_manifest_from_snapshot(
        evidence_paths["source_before_snapshot"], label="build-before"
    )
    after_issues, after_files = _source_manifest_from_snapshot(
        evidence_paths["source_after_snapshot"], label="build-after"
    )
    issues.extend(before_issues)
    issues.extend(after_issues)
    source_stable = bool(before_files) and before_files == after_files
    if not source_stable:
        issues.append("source manifest changed during the isolated Cargo build")
    cargo_config_stable = bool(cargo_config_before) and cargo_config_before == cargo_config_after
    if not cargo_config_stable:
        issues.append("Cargo config file inventory or hashes changed during the isolated build")

    executable: Path | None = None
    executable_hash: str | None = None
    artifact_count: int | None = None
    if cargo_exit == 0:
        try:
            executable, executable_hash = _parse_mimi_artifact(stdout, target_dir, host_target)
            artifact_count = 1
            _write_receipt_paths(gate_dir, executable, executable_hash)
        except (OSError, ValueError) as error:
            issues.append(str(error))
    evidence: dict[str, Any] = {}
    for name, path in evidence_paths.items():
        try:
            evidence[name] = _evidence_record(artifact_root, path)
        except (OSError, ValueError) as error:
            issues.append(f"could not hash raw build evidence {name}: {error}")
    if executable is not None and executable_hash is not None:
        for name, path in (
            ("cargo_mimi_path", gate_dir / "mimi.path"),
            ("cargo_mimi_sha256", gate_dir / "mimi.sha256"),
        ):
            try:
                evidence[name] = _evidence_record(artifact_root, path)
            except (OSError, ValueError) as error:
                issues.append(f"could not hash Cargo-reported Mimi evidence {name}: {error}")
    build_source_manifest = {
        "snapshot_id": _snapshot_id(before_files) if before_files else None,
        "manifest_sha256": source_manifest_digest(before_files) if before_files else None,
        "files": before_files,
    }
    cargo_config_attestation = {
        "stable_before_after": cargo_config_stable,
        "before": cargo_config_before,
        "after": cargo_config_after,
    }
    source_attestation_sha256 = source_attestation_digest(
        build_source_manifest["manifest_sha256"], cargo_config_attestation
    )
    receipt: dict[str, Any] = {
        "schema_version": RECEIPT_SCHEMA,
        "status": "verified" if not issues else "invalid",
        "current_source_attested": not issues,
        "root": str(root),
        "artifact_root": str(artifact_root),
        "cargo": {
            "argv": cargo_argv,
            "environment": cargo_environment,
            "environment_sha256": cargo_environment.get("environment_sha256"),
            "executable": cargo_identity_before,
            "exit_code": cargo_exit,
            "stdout_sha256": sha256_bytes(stdout),
            "stderr_sha256": sha256_bytes(stderr),
            "mimi_artifact_event_count": artifact_count,
        },
        "target": {
            "path": str(target_dir),
            "host_target": host_target,
            "fresh_directory": marker_values.get("created_exclusively") == "true"
            and marker_values.get("directory_existed_before") == "false",
        },
        "mimi": {
            "path": str(executable) if executable is not None else None,
            "sha256": executable_hash,
        },
        "build_source": {
            "stable_before_after": source_stable,
            "before_snapshot_id": _snapshot_id(before_files) if before_files else None,
            "after_snapshot_id": _snapshot_id(after_files) if after_files else None,
            "manifest": build_source_manifest,
            "cargo_config": cargo_config_attestation,
            "source_attestation_sha256": source_attestation_sha256,
        },
        "evidence": evidence,
        "issues": sorted(set(issues)),
    }
    if not receipt_path.is_absolute():
        receipt_path = artifact_root / receipt_path
    try:
        _write_json_exclusive(receipt_path, receipt)
    except OSError as error:
        raise ValueError(f"could not write build receipt without overwriting: {error}") from error
    return receipt, sorted(set(issues))


def _read_manifest_from_snapshot(value: Any, label: str) -> tuple[list[str], dict[str, dict[str, Any]]]:
    issues, files = validate_source_snapshot(value, label=label)
    return issues, files


def validate_receipt(
    receipt_value: Any,
    *,
    receipt_path: Path | None = None,
    root: Path | None = None,
    mimi_path: str | Path | None = None,
    report_snapshots: Mapping[str, Any] | None = None,
    outer_source_manifest: Mapping[str, Mapping[str, Any]] | None = None,
    expected_receipt_sha256: str | None = None,
    verify_receipt_file: bool = True,
) -> list[str]:
    """Recompute a receipt from raw evidence and compare it with supplied source sets."""
    issues: list[str] = []
    receipt = receipt_value if isinstance(receipt_value, dict) else {}
    if receipt.get("schema_version") != RECEIPT_SCHEMA:
        issues.append("build receipt has an unsupported schema")
    if receipt.get("status") != "verified" or receipt.get("current_source_attested") is not True:
        issues.append("build receipt is not marked as a verified current-source build")
    if receipt_path is None:
        issues.append("build receipt path is unavailable")
        artifact_root = None
    else:
        try:
            artifact_root_value = receipt.get("artifact_root")
            if not isinstance(artifact_root_value, str) or not Path(artifact_root_value).is_absolute():
                raise ValueError("receipt artifact_root is not absolute")
            artifact_root = Path(artifact_root_value).resolve(strict=True)
            receipt_resolved = receipt_path.resolve(strict=True)
            receipt_resolved.relative_to(artifact_root)
        except (OSError, ValueError):
            artifact_root = None
            issues.append("build receipt artifact root is absent or does not contain the receipt")
        if verify_receipt_file:
            try:
                current_receipt_sha256 = receipt_file_sha256(receipt_path)
            except (OSError, ValueError) as error:
                current_receipt_sha256 = None
                issues.append(f"build receipt is absent, non-regular, or symlinked: {error}")
            if (
                expected_receipt_sha256 is not None
                and current_receipt_sha256 != expected_receipt_sha256
            ):
                issues.append("build receipt file changed after its initial parse")
    evidence_value = receipt.get("evidence")
    if not isinstance(evidence_value, dict):
        evidence_value = {}
        issues.append("build receipt has no raw evidence file map")
    evidence_paths: dict[str, Path] = {}
    evidence_bytes: dict[str, bytes] = {}
    if artifact_root is not None:
        for name in (
            "cargo_argv", "cargo_environment", "cargo_identity_before", "cargo_config_before",
            "cargo_identity_after", "cargo_config_after",
            "environment_capture_argv", "environment_capture_exit",
            "environment_capture_stdout", "environment_capture_stderr",
            "source_before_capture_argv", "source_before_capture_exit", "source_before_capture_stdout",
            "source_before_capture_stderr", "source_after_capture_argv", "source_after_capture_exit",
            "source_after_capture_stdout", "source_after_capture_stderr",
            "cargo_identity_before_capture_argv", "cargo_identity_before_capture_exit",
            "cargo_identity_before_capture_stdout", "cargo_identity_before_capture_stderr",
            "cargo_config_before_capture_argv", "cargo_config_before_capture_exit",
            "cargo_config_before_capture_stdout", "cargo_config_before_capture_stderr",
            "cargo_identity_after_capture_argv", "cargo_identity_after_capture_exit",
            "cargo_identity_after_capture_stdout", "cargo_identity_after_capture_stderr",
            "cargo_config_after_capture_argv", "cargo_config_after_capture_exit",
            "cargo_config_after_capture_stdout", "cargo_config_after_capture_stderr",
            "cargo_exit", "cargo_stdout", "cargo_stderr",
            "fresh_target_marker", "source_before_snapshot", "source_after_snapshot",
            "cargo_mimi_path", "cargo_mimi_sha256",
        ):
            path, data, path_issues = _safe_evidence_file(
                artifact_root, evidence_value.get(name), name
            )
            issues.extend(path_issues)
            if path is not None:
                evidence_paths[name] = path
            if data is not None:
                evidence_bytes[name] = data
    try:
        argv = _read_argv_bytes(evidence_bytes["cargo_argv"])
    except (KeyError, OSError, ValueError) as error:
        argv = []
        issues.append(f"could not recompute Cargo argv from raw NUL evidence: {error}")
    cargo = receipt.get("cargo") if isinstance(receipt.get("cargo"), dict) else {}
    if cargo.get("argv") != argv:
        issues.append("receipt Cargo argv differs from the raw NUL argv file")
    try:
        environment = _json_object_bytes(evidence_bytes["cargo_environment"], "cargo environment")
    except (KeyError, OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        environment = {}
        issues.append(f"could not recompute Cargo environment from raw evidence: {error}")
    environment_issues, env_by_name = _validate_environment(environment)
    issues.extend(environment_issues)
    if cargo.get("environment") != environment:
        issues.append("receipt Cargo environment differs from the raw environment record")
    if cargo.get("environment_sha256") != environment.get("environment_sha256"):
        issues.append("receipt Cargo environment digest differs from the raw environment record")
    cargo_identity_before: dict[str, Any] = {}
    cargo_identity_after: dict[str, Any] = {}
    cargo_config_before: dict[str, Any] = {}
    cargo_config_after: dict[str, Any] = {}
    try:
        cargo_identity_before = _json_object_bytes(
            evidence_bytes["cargo_identity_before"], "pre-build Cargo identity"
        )
        cargo_identity_after = _json_object_bytes(
            evidence_bytes["cargo_identity_after"], "post-build Cargo identity"
        )
        cargo_config_before = _json_object_bytes(
            evidence_bytes["cargo_config_before"], "pre-build Cargo config"
        )
        cargo_config_after = _json_object_bytes(
            evidence_bytes["cargo_config_after"], "post-build Cargo config"
        )
    except (KeyError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        issues.append(f"could not recompute Cargo identity/config snapshots: {error}")
    if cargo_identity_before.get("schema_version") != CARGO_IDENTITY_SCHEMA:
        issues.append("raw pre-build Cargo identity has an unsupported schema")
    if cargo_identity_after.get("schema_version") != CARGO_IDENTITY_SCHEMA:
        issues.append("raw post-build Cargo identity has an unsupported schema")
    if cargo_identity_before != cargo_identity_after:
        issues.append("selected Cargo path/version/hash changed during the build")
    if argv and cargo_identity_before.get("path") != argv[0]:
        issues.append("Cargo identity path differs from raw argv[0]")
    try:
        recomputed_cargo_identity = (
            cargo_identity_document(root, Path(argv[0])) if root is not None and argv else None
        )
    except (OSError, ValueError) as error:
        recomputed_cargo_identity = None
        issues.append(f"raw Cargo argv[0] is not the selected rustup Cargo executable: {error}")
    if recomputed_cargo_identity is not None and cargo_identity_before != recomputed_cargo_identity:
        issues.append("receipt Cargo executable path/hash/version differs from current rustup Cargo")
    if cargo.get("executable") != cargo_identity_before:
        issues.append("receipt Cargo executable identity differs from raw pre-build evidence")
    if cargo_config_before.get("schema_version") != CARGO_CONFIG_SCHEMA:
        issues.append("raw pre-build Cargo config snapshot has an unsupported schema")
    if cargo_config_after.get("schema_version") != CARGO_CONFIG_SCHEMA:
        issues.append("raw post-build Cargo config snapshot has an unsupported schema")
    if cargo_config_before != cargo_config_after:
        issues.append("effective Cargo config files changed during the build")
    try:
        recomputed_cargo_config = (
            cargo_config_snapshot(root, _environment_values(environment))
            if root is not None
            else None
        )
    except (OSError, ValueError) as error:
        recomputed_cargo_config = None
        issues.append(f"could not recompute effective Cargo config files: {error}")
    if recomputed_cargo_config is not None and cargo_config_before != recomputed_cargo_config:
        issues.append("effective Cargo config files changed since the isolated build")
    capture_specs = {
        "environment_capture": [
            str(Path(__file__).resolve()), "capture-environment", "--output",
            str(evidence_paths.get("cargo_environment", "")),
        ],
        "source_before_capture": [
            str(Path(__file__).resolve()), "capture-source-snapshot", "--root", str(root.resolve()) if root else "",
            "--output", str(evidence_paths.get("source_before_snapshot", "")),
        ],
        "source_after_capture": [
            str(Path(__file__).resolve()), "capture-source-snapshot", "--root", str(root.resolve()) if root else "",
            "--output", str(evidence_paths.get("source_after_snapshot", "")),
        ],
        "cargo_identity_before_capture": [
            str(Path(__file__).resolve()), "capture-cargo-identity", "--root", str(root.resolve()) if root else "",
            "--cargo", argv[0] if argv else "", "--output",
            str(evidence_paths.get("cargo_identity_before", "")),
        ],
        "cargo_config_before_capture": [
            str(Path(__file__).resolve()), "capture-cargo-config", "--root", str(root.resolve()) if root else "",
            "--output", str(evidence_paths.get("cargo_config_before", "")),
        ],
        "cargo_identity_after_capture": [
            str(Path(__file__).resolve()), "capture-cargo-identity", "--root", str(root.resolve()) if root else "",
            "--cargo", argv[0] if argv else "", "--output",
            str(evidence_paths.get("cargo_identity_after", "")),
        ],
        "cargo_config_after_capture": [
            str(Path(__file__).resolve()), "capture-cargo-config", "--root", str(root.resolve()) if root else "",
            "--output", str(evidence_paths.get("cargo_config_after", "")),
        ],
    }
    for prefix, expected_suffix in capture_specs.items():
        evidence_prefix = prefix.replace("_", "-")
        try:
            raw_argv = _read_argv_bytes(evidence_bytes[f"{prefix}_argv"])
            capture_exit = _read_exit_bytes(evidence_bytes[f"{prefix}_exit"], prefix)
            capture_stdout = evidence_bytes[f"{prefix}_stdout"]
            capture_stderr = evidence_bytes[f"{prefix}_stderr"]
            if raw_argv[-len(expected_suffix):] != expected_suffix:
                issues.append(f"raw {evidence_prefix} argv differs from the recorded capture inputs")
            if capture_exit != 0 or capture_stdout or capture_stderr:
                issues.append(f"raw {evidence_prefix} capture did not exit cleanly with empty stdout/stderr")
        except (KeyError, OSError, UnicodeError, ValueError) as error:
            issues.append(f"could not recompute raw {evidence_prefix} command: {error}")
    try:
        exit_code = _read_exit_bytes(evidence_bytes["cargo_exit"], "cargo_exit")
    except (KeyError, OSError, UnicodeError, ValueError) as error:
        exit_code = None
        issues.append(f"could not recompute Cargo exit code from raw evidence: {error}")
    if cargo.get("exit_code") != exit_code or exit_code != 0:
        issues.append("receipt Cargo exit code is not the successful raw exit code")
    try:
        stdout = evidence_bytes["cargo_stdout"]
        stderr = evidence_bytes["cargo_stderr"]
    except (KeyError, OSError) as error:
        stdout = b""
        stderr = b""
        issues.append(f"could not read raw Cargo stdout/stderr: {error}")
    if cargo.get("stdout_sha256") != sha256_bytes(stdout):
        issues.append("receipt raw Cargo JSON stdout digest differs")
    if cargo.get("stderr_sha256") != sha256_bytes(stderr):
        issues.append("receipt raw Cargo stderr digest differs")
    if cargo.get("mimi_artifact_event_count") != 1:
        issues.append("receipt does not record exactly one Cargo-reported Mimi artifact")

    target = receipt.get("target") if isinstance(receipt.get("target"), dict) else {}
    try:
        target_path = Path(str(target.get("path"))).resolve(strict=True)
    except OSError:
        target_path = None
        issues.append("receipt isolated Cargo target path is unavailable")
    if target.get("fresh_directory") is not True:
        issues.append("receipt does not prove a fresh isolated target directory")
    if target_path is not None:
        if env_by_name.get("CARGO_TARGET_DIR", {}).get("value") != str(target_path):
            issues.append("raw Cargo environment target differs from receipt target path")
        if env_by_name.get("CARGO_BUILD_JOBS", {}).get("value") != "1":
            issues.append("raw Cargo environment does not fix CARGO_BUILD_JOBS=1")
        if env_by_name.get("CARGO_TERM_COLOR", {}).get("value") != "never":
            issues.append("raw Cargo environment does not fix CARGO_TERM_COLOR=never")
    if root is not None:
        env_record = environment if isinstance(environment, dict) else {}
        if env_record.get("working_directory") != str(root.resolve()):
            issues.append("raw Cargo working directory differs from the census root")
    host_target = target.get("host_target")
    if not isinstance(host_target, str) or not host_target:
        issues.append("receipt native host target is missing")
    expected_cargo_args = [
        "build", "--bin", "mimi", "--locked", "--target", host_target,
        "--message-format=json-render-diagnostics",
    ] if isinstance(host_target, str) else []
    if len(argv) < 2 or argv[1:] != expected_cargo_args:
        issues.append("raw Cargo argv does not match the fixed isolated Mimi build")
    marker_path = evidence_paths.get("fresh_target_marker")
    if marker_path is not None:
        try:
            marker = dict(
                line.split("=", 1)
                for line in evidence_bytes["fresh_target_marker"].decode("utf-8").splitlines()
                if "=" in line
            )
            if (
                marker.get("target_path") != str(target_path)
                or marker.get("directory_existed_before") != "false"
                or marker.get("created_exclusively") != "true"
            ):
                issues.append("raw target marker does not prove a fresh isolated target")
        except (OSError, UnicodeError, ValueError) as error:
            issues.append(f"could not parse raw fresh-target marker: {error}")

    expected_executable: Path | None = None
    expected_executable_hash: str | None = None
    if target_path is not None and isinstance(host_target, str):
        try:
            expected_executable, expected_executable_hash = _parse_mimi_artifact(
                stdout, target_path, host_target
            )
        except (OSError, ValueError) as error:
            issues.append(f"raw Cargo JSON does not identify one in-target Mimi executable: {error}")
    mimi = receipt.get("mimi") if isinstance(receipt.get("mimi"), dict) else {}
    try:
        raw_mimi_path = evidence_bytes["cargo_mimi_path"].decode("utf-8").strip()
        raw_mimi_hash = evidence_bytes["cargo_mimi_sha256"].decode("ascii").strip()
    except (KeyError, OSError, UnicodeError) as error:
        raw_mimi_path, raw_mimi_hash = "", ""
        issues.append(f"could not read Cargo-reported Mimi path/hash evidence: {error}")
    if expected_executable is None or expected_executable_hash is None:
        issues.append("Cargo-reported Mimi executable identity cannot be recomputed")
    else:
        if str(expected_executable) != raw_mimi_path:
            issues.append("raw Cargo-reported executable path differs from its captured path file")
        if expected_executable_hash != raw_mimi_hash:
            issues.append("raw Cargo-reported executable SHA-256 differs from its captured hash file")
        if mimi.get("path") != str(expected_executable) or mimi.get("sha256") != expected_executable_hash:
            issues.append("receipt Mimi identity differs from the Cargo JSON artifact and executable bytes")
        try:
            current_hash = sha256_file(expected_executable)
        except OSError:
            current_hash = None
        # expected_executable_hash is recomputed from the current file bytes by
        # _parse_mimi_artifact. Compare the current bytes with the hash captured
        # immediately after Cargo instead of comparing that value to itself.
        if current_hash != raw_mimi_hash:
            issues.append("Cargo-reported Mimi executable changed after the build")
        if mimi_path is not None:
            try:
                requested_path = Path(mimi_path).resolve(strict=True)
            except OSError:
                requested_path = None
            if requested_path != expected_executable:
                issues.append("census --mimi path differs from the receipt Cargo executable")
    if root is not None and receipt.get("root") != str(root.resolve()):
        issues.append("build receipt repository root differs from the census root")

    snapshots: dict[str, dict[str, dict[str, Any]]] = {}
    for name in ("source_before_snapshot", "source_after_snapshot"):
        snapshot_data = evidence_bytes.get(name)
        if snapshot_data is None:
            continue
        snapshot_issues, snapshot_files = _source_manifest_from_snapshot_bytes(
            snapshot_data, label=name
        )
        issues.extend(snapshot_issues)
        snapshots[name] = snapshot_files
    if not snapshots.get("source_before_snapshot") or not snapshots.get("source_after_snapshot"):
        issues.append("raw build source snapshots are missing")
    elif snapshots["source_before_snapshot"] != snapshots["source_after_snapshot"]:
        issues.append("raw build source snapshots differ before and after Cargo")
    build_source = receipt.get("build_source") if isinstance(receipt.get("build_source"), dict) else {}
    manifest_value = build_source.get("manifest") if isinstance(build_source.get("manifest"), dict) else {}
    manifest_files_value = manifest_value.get("files")
    if not isinstance(manifest_files_value, dict):
        manifest_files_value = {}
        issues.append("receipt build-source manifest is missing")
    if manifest_files_value != snapshots.get("source_before_snapshot", {}):
        issues.append("receipt build-source manifest differs from the raw pre-build source snapshot")
    if build_source.get("stable_before_after") is not True:
        issues.append("receipt does not prove stable build sources before and after Cargo")
    if manifest_value.get("snapshot_id") != build_source.get("before_snapshot_id"):
        issues.append("receipt build-source snapshot identity is inconsistent")
    if build_source.get("before_snapshot_id") != build_source.get("after_snapshot_id"):
        issues.append("receipt build-source before/after snapshot IDs differ")
    try:
        computed_manifest_digest = source_manifest_digest(manifest_files_value)
    except (TypeError, ValueError):
        computed_manifest_digest = None
    if manifest_value.get("manifest_sha256") != computed_manifest_digest:
        issues.append("receipt build-source manifest digest does not recompute")
    config_attestation = build_source.get("cargo_config")
    expected_config_attestation = {
        "stable_before_after": cargo_config_before == cargo_config_after,
        "before": cargo_config_before,
        "after": cargo_config_after,
    }
    if config_attestation != expected_config_attestation:
        issues.append("receipt Cargo config attestation differs from raw before/after snapshots")
    if not isinstance(config_attestation, dict) or config_attestation.get("stable_before_after") is not True:
        issues.append("receipt does not prove stable effective Cargo config files")
    expected_source_attestation = source_attestation_digest(
        computed_manifest_digest,
        expected_config_attestation,
    )
    if build_source.get("source_attestation_sha256") != expected_source_attestation:
        issues.append("receipt combined source/config attestation digest does not recompute")

    if outer_source_manifest is not None:
        shared_paths = set(outer_source_manifest) & set(manifest_files_value)
        if any(
            outer_source_manifest[path].get("sha256") != manifest_files_value[path].get("sha256")
            for path in shared_paths
        ):
            issues.append("runner outer and collector source hashes differ on shared paths")
    if report_snapshots is not None:
        for report_label, snapshot_value in report_snapshots.items():
            report_issues, report_files = validate_source_snapshot(
                snapshot_value, label=f"full-route {report_label}"
            )
            issues.extend(report_issues)
            if report_files != manifest_files_value:
                issues.append(
                    f"full-route {report_label}.files differs path-by-path from the build receipt"
                )

    return sorted(set(issues))


def capture_collector_snapshot(root: Path, output: Path) -> dict[str, Any]:
    scripts_dir = Path(__file__).resolve().parent
    if str(scripts_dir) not in sys.path:
        sys.path.insert(0, str(scripts_dir))
    import full_route_census  # type: ignore[import-not-found]

    resolved_root = root.resolve(strict=True)
    tracked_paths, _tracked_inventory = full_route_census.tracked_mimi_paths(resolved_root)
    sources, _scope_exclusions, _source_scope = full_route_census.enumerate_source_scope(
        resolved_root, tracked_paths
    )
    snapshot = full_route_census.source_snapshot(resolved_root, sources)
    snapshot_issues, _files = validate_source_snapshot(snapshot, label="captured collector")
    if snapshot_issues:
        raise ValueError("; ".join(snapshot_issues))
    _write_json_exclusive(output, snapshot)
    return snapshot


def _cli() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    environment_parser = subparsers.add_parser("capture-environment")
    environment_parser.add_argument("--output", required=True, type=Path)
    snapshot_parser = subparsers.add_parser("capture-source-snapshot")
    snapshot_parser.add_argument("--root", required=True, type=Path)
    snapshot_parser.add_argument("--output", required=True, type=Path)
    cargo_identity_parser = subparsers.add_parser("capture-cargo-identity")
    cargo_identity_parser.add_argument("--root", required=True, type=Path)
    cargo_identity_parser.add_argument("--cargo", required=True, type=Path)
    cargo_identity_parser.add_argument("--output", required=True, type=Path)
    cargo_config_parser = subparsers.add_parser("capture-cargo-config")
    cargo_config_parser.add_argument("--root", required=True, type=Path)
    cargo_config_parser.add_argument("--output", required=True, type=Path)
    create_parser = subparsers.add_parser("create")
    create_parser.add_argument("--artifact-root", required=True, type=Path)
    create_parser.add_argument("--gate-dir", required=True, type=Path)
    create_parser.add_argument("--target-dir", required=True, type=Path)
    create_parser.add_argument("--host-target", required=True)
    create_parser.add_argument("--root", required=True, type=Path)
    create_parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "capture-environment":
            _write_json_exclusive(args.output, environment_document(os.environ))
            return 0
        if args.command == "capture-source-snapshot":
            capture_collector_snapshot(args.root, args.output)
            return 0
        if args.command == "capture-cargo-identity":
            capture_cargo_identity(args.root, args.cargo, args.output)
            return 0
        if args.command == "capture-cargo-config":
            capture_cargo_config(args.root, args.output)
            return 0
        receipt, issues = create_receipt(
            artifact_root=args.artifact_root,
            gate_dir=args.gate_dir,
            target_dir=args.target_dir,
            host_target=args.host_target,
            root=args.root,
            receipt_path=args.receipt,
        )
        print(f"receipt_status={receipt['status']}")
        for issue in issues:
            print(f"FAIL {issue}", file=sys.stderr)
        return 0 if not issues else 1
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        print(f"[actor-census-build-receipt] error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(_cli())
