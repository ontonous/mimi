#!/usr/bin/env python3
"""Source-bound artifact manifest for the generated stdlib probe matrix.

The matrix is generated outside the checkout.  This module ties that external
directory back to the conceptual ``tests/stdlib_matrix_generated/...`` source
paths consumed by route census tooling, and refuses stale, incomplete, or
hand-edited output.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
from typing import Any, Mapping


SCHEMA = "mimi-stdlib-matrix-artifact-v1"
MANIFEST_NAME = "stdlib-matrix-manifest-v1.json"
GENERATOR_PATH = "scripts/gen-stdlib-matrix.py"
SOURCE_PREFIX = "tests/stdlib_matrix_generated/"
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")


class MatrixArtifactError(ValueError):
    """The external matrix artifact is invalid or stale."""


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def _reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise MatrixArtifactError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise MatrixArtifactError(f"non-finite JSON number is not allowed: {value}")


def _regular_file(path: Path, *, label: str) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise MatrixArtifactError(f"{label} is unavailable: {path}") from error
    if not stat.S_ISREG(metadata.st_mode) or path.is_symlink():
        raise MatrixArtifactError(f"{label} must be a regular non-symlink file: {path}")
    try:
        return path.read_bytes()
    except OSError as error:
        raise MatrixArtifactError(f"{label} is unreadable: {path}") from error


def _normalized_relative(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or not value or "\\" in value:
        raise MatrixArtifactError(f"{label} must be a normalized relative POSIX path")
    path = PurePosixPath(value)
    if path.is_absolute() or path.as_posix() != value or any(part in {"", ".", ".."} for part in path.parts):
        raise MatrixArtifactError(f"{label} must be a normalized relative POSIX path: {value!r}")
    return value


def _repository_root(root: Path) -> Path:
    try:
        resolved = root.resolve(strict=True)
    except OSError as error:
        raise MatrixArtifactError(f"repository root is unavailable: {root}") from error
    if not resolved.is_dir() or root.is_symlink():
        raise MatrixArtifactError(f"repository root must be a regular directory: {root}")
    return resolved


def _external_directory(path: Path, root: Path, *, must_exist: bool) -> Path:
    candidate = Path(path).expanduser()
    if candidate.is_symlink():
        raise MatrixArtifactError(f"generated root must not be a symlink: {candidate}")
    try:
        resolved = candidate.resolve(strict=must_exist)
    except OSError as error:
        raise MatrixArtifactError(f"generated root is unavailable: {candidate}") from error
    if must_exist and not resolved.is_dir():
        raise MatrixArtifactError(f"generated root must be a directory: {resolved}")
    try:
        resolved.relative_to(root)
    except ValueError:
        return resolved
    raise MatrixArtifactError(f"generated root must be outside the repository: {resolved}")


def _stdlib_inputs(root: Path) -> list[dict[str, Any]]:
    directory = root / "std"
    if not directory.is_dir() or directory.is_symlink():
        raise MatrixArtifactError("stdlib input directory is missing or symlinked")
    result: list[dict[str, Any]] = []
    for path in sorted(directory.glob("*.mimi"), key=lambda item: item.name):
        raw = _regular_file(path, label="stdlib input")
        result.append({
            "path": f"std/{path.name}",
            "bytes": len(raw),
            "sha256": sha256_bytes(raw),
        })
    if not result:
        raise MatrixArtifactError("stdlib matrix generation requires at least one std/*.mimi input")
    return result


def _load_generator(root: Path) -> Any:
    path = root / GENERATOR_PATH
    _regular_file(path, label="matrix generator")
    scripts_dir = str(path.parent)
    if scripts_dir not in sys.path:
        sys.path.insert(0, scripts_dir)
    spec = importlib.util.spec_from_file_location("mimi_stdlib_matrix_generator_for_artifact", path)
    if spec is None or spec.loader is None:
        raise MatrixArtifactError("cannot load stdlib matrix generator")
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except Exception as error:
        raise MatrixArtifactError(f"cannot evaluate stdlib matrix generator: {error}") from error
    render = getattr(module, "render_sources", None)
    if not callable(render):
        raise MatrixArtifactError("matrix generator does not expose deterministic render_sources()")
    return module


def _expected_outputs(root: Path) -> dict[str, bytes]:
    module = _load_generator(root)
    try:
        outputs = module.render_sources()
    except Exception as error:
        raise MatrixArtifactError(f"matrix generator failed to render deterministic outputs: {error}") from error
    if not isinstance(outputs, Mapping) or not outputs:
        raise MatrixArtifactError("matrix generator returned no output mapping")
    result: dict[str, bytes] = {}
    for raw_path, raw_content in outputs.items():
        artifact_path = _normalized_relative(raw_path, label="generated artifact path")
        if not artifact_path.endswith(".mimi") or not isinstance(raw_content, str):
            raise MatrixArtifactError(f"generator returned a non-Mimi artifact: {artifact_path}")
        result[artifact_path] = raw_content.encode("utf-8")
    if len(result) != len(outputs):
        raise MatrixArtifactError("matrix generator returned duplicate output paths")
    return dict(sorted(result.items()))


def _inventory_generated_files(generated_root: Path) -> dict[str, bytes]:
    result: dict[str, bytes] = {}
    for directory, dirnames, filenames in os.walk(generated_root, followlinks=False):
        current = Path(directory)
        for dirname in list(dirnames):
            child = current / dirname
            if child.is_symlink():
                raise MatrixArtifactError(f"generated artifact directory is a symlink: {child}")
        dirnames[:] = sorted(dirnames)
        for filename in sorted(filenames):
            path = current / filename
            rel = path.relative_to(generated_root).as_posix()
            if path.is_symlink():
                raise MatrixArtifactError(f"generated artifact file is a symlink: {path}")
            if rel == MANIFEST_NAME:
                continue
            if not rel.endswith(".mimi"):
                raise MatrixArtifactError(f"unexpected non-Mimi file in generated root: {rel}")
            result[rel] = _regular_file(path, label="generated matrix source")
    return dict(sorted(result.items()))


def _manifest_document(root: Path, outputs: Mapping[str, bytes]) -> dict[str, Any]:
    generator = root / GENERATOR_PATH
    generator_raw = _regular_file(generator, label="matrix generator")
    inputs = _stdlib_inputs(root)
    files = [
        {
            "path": SOURCE_PREFIX + artifact_path,
            "artifact_path": artifact_path,
            "bytes": len(content),
            "sha256": sha256_bytes(content),
        }
        for artifact_path, content in sorted(outputs.items())
    ]
    roster = [{"path": item["path"], "sha256": item["sha256"]} for item in files]
    return {
        "schema": SCHEMA,
        "source_path_prefix": SOURCE_PREFIX,
        "generator": {"path": GENERATOR_PATH, "sha256": sha256_bytes(generator_raw)},
        "stdlib_inputs": inputs,
        "stdlib_inputs_sha256": sha256_bytes(canonical_bytes(inputs)),
        "generated_file_count": len(files),
        "generated_roster_sha256": sha256_bytes(canonical_bytes(roster)),
        "generated_files": files,
    }


def write_manifest(root: Path, generated_root: Path, outputs: Mapping[str, bytes]) -> Path:
    """Write the deterministic manifest after generator-owned files are emitted."""
    repo_root = _repository_root(root)
    external_root = _external_directory(generated_root, repo_root, must_exist=True)
    manifest_path = external_root / MANIFEST_NAME
    if manifest_path.exists() or manifest_path.is_symlink():
        raise MatrixArtifactError(f"refusing to overwrite matrix manifest: {manifest_path}")
    expected = {key: bytes(value) for key, value in sorted(outputs.items())}
    actual = _inventory_generated_files(external_root)
    if actual != expected:
        raise MatrixArtifactError("written matrix outputs do not exactly match generator render_sources()")
    manifest_path.write_bytes(canonical_bytes(_manifest_document(repo_root, expected)))
    return manifest_path


def verify(root: Path, generated_root: Path) -> dict[str, Any]:
    """Verify the external artifact against current generator and stdlib bytes."""
    repo_root = _repository_root(Path(root))
    external_root = _external_directory(Path(generated_root), repo_root, must_exist=True)
    manifest_path = external_root / MANIFEST_NAME
    raw_manifest = _regular_file(manifest_path, label="stdlib matrix manifest")
    try:
        document = json.loads(
            raw_manifest.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_pairs,
            parse_constant=_reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise MatrixArtifactError(f"matrix manifest is not valid UTF-8 JSON: {error}") from error
    if not isinstance(document, dict) or document.get("schema") != SCHEMA:
        raise MatrixArtifactError(f"matrix manifest schema must be {SCHEMA}")

    expected_outputs = _expected_outputs(repo_root)
    expected_document = _manifest_document(repo_root, expected_outputs)
    if document != expected_document:
        raise MatrixArtifactError("matrix manifest is stale or disagrees with current generator/stdlib inputs")
    actual_outputs = _inventory_generated_files(external_root)
    if actual_outputs != expected_outputs:
        missing = sorted(set(expected_outputs) - set(actual_outputs))
        extra = sorted(set(actual_outputs) - set(expected_outputs))
        changed = sorted(
            path for path in set(actual_outputs) & set(expected_outputs)
            if actual_outputs[path] != expected_outputs[path]
        )
        raise MatrixArtifactError(
            f"generated matrix bytes differ from deterministic generator output; missing={missing[:5]} extra={extra[:5]} changed={changed[:5]}"
        )

    entries = {
        str(item["path"]): {
            "path": str(item["path"]),
            "artifact_path": str(item["artifact_path"]),
            "bytes": int(item["bytes"]),
            "sha256": str(item["sha256"]),
            "external_path": str(external_root / str(item["artifact_path"])),
        }
        for item in document["generated_files"]
    }
    return {
        "schema": SCHEMA,
        "generated_root": str(external_root),
        "manifest_path": str(manifest_path),
        "manifest_sha256": sha256_bytes(raw_manifest),
        "generator_path": GENERATOR_PATH,
        "generator_sha256": document["generator"]["sha256"],
        "stdlib_inputs_count": len(document["stdlib_inputs"]),
        "stdlib_inputs_sha256": document["stdlib_inputs_sha256"],
        "generated_file_count": document["generated_file_count"],
        "generated_roster_sha256": document["generated_roster_sha256"],
        "files": entries,
    }


def main(argv: list[str] | None = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    verify_parser = subparsers.add_parser("verify", help="verify generated bytes and current source inputs")
    verify_parser.add_argument("--root", type=Path, required=True)
    verify_parser.add_argument("--generated-root", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        record = verify(args.root, args.generated_root)
    except (MatrixArtifactError, OSError, ValueError) as error:
        print(f"stdlib matrix artifact verification failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps({key: value for key, value in record.items() if key != "files"}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
