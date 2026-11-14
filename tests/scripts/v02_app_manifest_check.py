#!/usr/bin/env python3
"""Strictly validate the V0.2 W1-W4 fixture manifest and its frozen inputs.

This check validates only the fixture contract: exact artifact inventory,
content hashes, source closures, independent oracle references, required
negative cases, and cumulative sequence declarations. It does not invoke Mimi,
Cargo, C compilers, or claim that any fixture has passed.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path, PurePosixPath
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests" / "fixtures" / "v02_app"
MANIFEST_PATH = FIXTURES / "manifest.json"
SHA256_LENGTH = 64

APP_IDS = ("W1", "W2", "W3", "W4")
CONTRACT_DOCUMENTS = (
    "devdocs/V0.2/README.md",
    "devdocs/V0.2/plan.toml",
    "devdocs/V0.2/02-阶段与任务.md",
    "devdocs/V0.2/03-验收程序与评测.md",
    "devdocs/V0.2/08-执行与证据合同.md",
)
CHANGE_IDS = (
    "C1", "C2", "C3", "C4",
    "D1", "D2", "D3", "D4", "D5",
    "H1", "H2", "H3",
    "S1", "S2", "S3", "S4",
)
SEQUENCE_STEPS = {
    "SEQ-W2": ("W2", "D1", "D2", "D3", "D4", "D5"),
    "SEQ-W4": ("W4", "S1", "S2", "S3", "S4"),
}
PATCH_PATHS = {
    **{f"D{i}": f"w2/sequences/patches/D{i}.patch" for i in range(1, 6)},
    **{f"S{i}": f"w4/sequences/patches/S{i}.patch" for i in range(1, 5)},
}
APP_SOURCE_CLOSURES = {
    "W1": {
        "w1/main.mimi",
        "w1/apps/config_json.mimi",
        "w1/apps/record_json.mimi",
    },
    "W2": {"w2/w2_document_core.mimi"},
    "W3": {"w3/audio_kernel.mimi"},
    "W4": {
        "w4/baseline/src/main.mimi",
        "w4/baseline/src/protocol.mimi",
        "w4/baseline/src/server.mimi",
        "w4/baseline/src/exports.mimi",
        "w4/baseline/native/w4_trusted.c",
    },
}
APP_SOURCE_INPUTS = {
    "W1": None,  # sample inputs are enumerated exactly by the manifest tree.
    "W2": [],
    "W3": ["w3/oracle_vectors.mimi"],
    "W4": ["w4/baseline/mimi.toml", "w4/oracle/w4_expected.json", "w4/oracle/w4_ffi_contract.json"],
}
APP_ORACLES = {
    "W1": ("w1_scoped_cli", "oracle/w1_expected.json", "cases"),
    "W2": ("mimi_stdout", "oracle/w2_expected.json", "baseline"),
    "W3": ("mimi_stdout", "oracle/w3_expected.json", "baseline"),
    "W4": ("w4_host_driver", "w4/oracle/w4_expected.json", "$"),
}
CHANGE_SOURCE_CLOSURES = {
    "C1": (None, ()), "C2": (None, ()), "C3": (None, ()), "C4": (None, ()),
    "D1": ("w2/changes/D1_w2_delta_snapshot.mimi", ("w2/changes/D1_w2_delta_snapshot.mimi",)),
    "D2": ("w2/changes/D2_w2_cancel_during_write.mimi", ("w2/changes/D2_w2_cancel_during_write.mimi",)),
    "D3": ("w2/changes/D3_w2_error_variant.mimi", ("w2/changes/D3_w2_error_variant.mimi",)),
    "D4": ("w2/changes/D4_w2_export_module_boundary.mimi", (
        "w2/changes/D4_w2_export_module_boundary.mimi", "w2/changes/w2_export_io.mimi",
    )),
    "D5": ("w2/changes/D5_w2_two_background_tasks.mimi", ("w2/changes/D5_w2_two_background_tasks.mimi",)),
    "H1": ("w3/H1_gain_algorithm.mimi", ("w3/H1_gain_algorithm.mimi",)),
    "H2": ("w3/H2_optional_filter.mimi", ("w3/H2_optional_filter.mimi",)),
    "H3": ("w3/H3_shared_snapshot.mimi", ("w3/H3_shared_snapshot.mimi",)),
    "S1": ("w4/changes/S1/w4_trusted_s1.c", (
        "w4/baseline/src/main.mimi", "w4/baseline/src/protocol.mimi",
        "w4/baseline/src/server.mimi", "w4/baseline/src/exports.mimi",
        "w4/baseline/native/w4_trusted.c", "w4/changes/S1/w4_trusted_s1.c",
    )),
    "S2": ("w4/changes/S2/timeout_handoff.mimi", ("w4/changes/S2/timeout_handoff.mimi",)),
    "S3": ("w4/changes/S3/endpoint_scope.mimi", (
        "w4/changes/S3/endpoint_scope.mimi", "w4/changes/S3/w4_endpoint_scope_s3.c",
        "w4/changes/S3/endpoint_scope_oracle.c",
    )),
    "S4": ("w4/offline-release/src/main.mimi", (
        "w4/offline-release/src/main.mimi", "w4/offline-release/src/protocol.mimi",
        "w4/offline-release/src/server.mimi", "w4/offline-release/src/exports.mimi",
        "w4/offline-release/.mimi/deps/w4_transport/transport.mimi",
    )),
}
CHANGE_SOURCE_INPUTS = {
    "S4": (
        "w4/offline-release/mimi.toml",
        "w4/offline-release/mimi.lock",
        "w4/offline-release/.mimi/deps/w4_transport/mimi.toml",
    ),
}
CHANGE_ORACLES = {
    **{f"C{i}": ("w1_scoped_cli", "oracle/w1_expected.json", f"requirements/C{i}") for i in range(1, 5)},
    **{f"D{i}": ("mimi_stdout", "oracle/w2_expected.json", f"changes/D{i}") for i in range(1, 6)},
    **{f"H{i}": ("mimi_stdout", "oracle/w3_expected.json", f"changes/H{i}") for i in range(1, 4)},
    "S1": ("w4_host_driver", "w4/oracle/w4_expected.json", "$"),
    "S2": ("mimi_stdout", "oracle/w4_changes_expected.json", "S2"),
    "S3": ("w4_endpoint_scope", "oracle/w4_changes_expected.json", "S3/endpoint_scope"),
    "S4": ("w4_host_driver", "w4/oracle/w4_expected.json", "$"),
}
NEGATIVE_IDS = {
    "W1_CONFIG_DUPLICATE",
    "W1_CONFIG_DUPLICATE_ESCAPED",
    "W1_CONFIG_EMPTY",
    "W1_CONFIG_MALFORMED",
    "W1_CONFIG_NUL",
    "W1_RECORD_DUPLICATE",
    "W1_RECORD_EMPTY",
    "W1_RECORD_MALFORMED",
    "W1_RECORD_QUOTED_AMOUNT",
    "W1_RECORD_NUL",
    "W1_MISSING_RECORD",
    "W1_CONFIG_READ_MISSING",
    "W1_PATH_ESCAPE",
    "W1_WRITE_FAILURE",
    "W1_PREVIEW_CUMULATIVE",
    "W2_CLOSED_EDIT",
    "W2_UNRESPONSIVE_HANDOFF",
    "W2_FLOW_PROTOCOL",
    "W3_NOALLOC_GROWTH",
    "W3_NOALLOC_TRANSFORM",
    "W3_NOALLOC_LIVE_SNAPSHOT",
    "W3_NOALLOC_UNKNOWN_EFFECT",
    "W3_NOALLOC_ERROR_BOUNDS",
    "W4_WRONG_SCALAR_ARGUMENT",
    "W4_WRONG_EXTERNAL_ABI",
    "W4_BORROWED_STRING_OWNERSHIP",
    "W4_S3_DENIED_ENDPOINTS",
}
NEGATIVE_BINDINGS = {
    "W1_CONFIG_DUPLICATE": (None, ("w1/sample/cases/config_duplicate.json",), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/config_duplicate")),
    "W1_CONFIG_DUPLICATE_ESCAPED": (None, ("w1/sample/cases/config_duplicate_escaped_key.json",), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/config_duplicate_escaped_key")),
    "W1_CONFIG_EMPTY": (None, ("w1/sample/cases/config_empty.json",), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/config_empty")),
    "W1_CONFIG_MALFORMED": (None, ("w1/sample/cases/config_malformed.json",), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/config_malformed")),
    "W1_CONFIG_NUL": (None, ("w1/sample/cases/config_unicode_nul.json",), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/config_unicode_nul")),
    "W1_RECORD_DUPLICATE": (None, ("w1/sample/cases/config_record_duplicate.json", "w1/sample/cases/record_duplicate.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/record_duplicate")),
    "W1_RECORD_EMPTY": (None, ("w1/sample/cases/config_record_empty.json", "w1/sample/cases/record_empty.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/record_empty")),
    "W1_RECORD_MALFORMED": (None, ("w1/sample/cases/config_record_malformed.json", "w1/sample/cases/record_malformed.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/record_malformed")),
    "W1_RECORD_QUOTED_AMOUNT": (None, ("w1/sample/cases/config_record_quoted_amount.json", "w1/sample/cases/record_quoted_amount.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/record_quoted_amount")),
    "W1_RECORD_NUL": (None, ("w1/sample/cases/config_record_unicode_nul.json", "w1/sample/cases/record_unicode_nul.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/record_unicode_nul")),
    "W1_MISSING_RECORD": (None, ("w1/sample/cases/config_missing_record.json",), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/record_missing")),
    "W1_CONFIG_READ_MISSING": (None, (), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/config_read_missing")),
    "W1_PATH_ESCAPE": (None, ("w1/sample/cases/config_path_escape.json", "w1/sample/outside/secret.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/path_escape")),
    "W1_WRITE_FAILURE": (None, ("w1/sample/config/config.json", "w1/sample/records/alpha.json", "w1/sample/records/beta.json"), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/write_failure")),
    "W1_PREVIEW_CUMULATIVE": (None, (), ("w1_scoped_cli", "oracle/w1_expected.json", "cases/preview")),
    "W2_CLOSED_EDIT": ("w2/w2_closed_edit_rejected.mimi", (), ("mimi_check_reject", "oracle/w2_expected.json", "negative/closed_session_edit_rejected")),
    "W2_UNRESPONSIVE_HANDOFF": ("w2/w2_unresponsive_handoff.mimi", (), ("mimi_stdout", "oracle/w2_expected.json", "negative/unresponsive_handoff")),
    "W2_FLOW_PROTOCOL": ("w2/w2_flow_protocol.mimi", (), ("mimi_stdout", "oracle/w2_expected.json", "negative/flow_protocol")),
    "W3_NOALLOC_GROWTH": (None, (), ("blocked_static_negative", "oracle/w3_expected.json", "no_alloc_negative_requirements/push_pop_growth")),
    "W3_NOALLOC_TRANSFORM": (None, (), ("blocked_static_negative", "oracle/w3_expected.json", "no_alloc_negative_requirements/transform_collection")),
    "W3_NOALLOC_LIVE_SNAPSHOT": (None, (), ("blocked_static_negative", "oracle/w3_expected.json", "no_alloc_negative_requirements/live_snapshot_cow")),
    "W3_NOALLOC_UNKNOWN_EFFECT": (None, (), ("blocked_static_negative", "oracle/w3_expected.json", "no_alloc_negative_requirements/unknown_callback_or_ffi")),
    "W3_NOALLOC_ERROR_BOUNDS": (None, (), ("blocked_static_negative", "oracle/w3_expected.json", "no_alloc_negative_requirements/error_and_bounds_paths")),
    "W4_WRONG_SCALAR_ARGUMENT": ("w4/negative/wrong_scalar_argument.mimi", (), ("mimi_check_reject", "oracle/w4_changes_expected.json", "negatives/wrong_scalar_argument")),
    "W4_WRONG_EXTERNAL_ABI": ("w4/negative/wrong_external_abi_declaration.mimi", (), ("blocked_static_negative", "oracle/w4_changes_expected.json", "negatives/wrong_external_abi")),
    "W4_BORROWED_STRING_OWNERSHIP": ("w4/negative/borrowed_c_string_as_owned.mimi", (), ("blocked_static_negative", "oracle/w4_changes_expected.json", "negatives/borrowed_string_ownership")),
    "W4_S3_DENIED_ENDPOINTS": ("w4/changes/S3/endpoint_scope.mimi", (), ("w4_endpoint_scope", "oracle/w4_changes_expected.json", "S3/endpoint_scope")),
}
LEGACY_PATHS = {
    "w1_config_tool.mimi",
    "w2_flow_probe.mimi",
    "w3_audio_kernel.mimi",
    "w4_concurrency_probe.mimi",
    "w4_fault_probe.mimi",
    "w4_loopback_service.mimi",
    "w4_network_probe.mimi",
    "oracle/v02_expected.json",
    "oracle/v02_ffi.c",
    "oracle/native_fault_control.c",
    "oracle/README.md",
    "changes/C1_w1_parser_replacement.mimi",
    "changes/C2_w1_source_field.mimi",
    "changes/C3_w1_permission_scope.mimi",
    "changes/C4_w1_read_only_preview.mimi",
    "changes/D1_w2_delta_snapshot.mimi",
    "changes/D2_w2_cancel_during_write.mimi",
    "changes/D3_w2_error_variant.mimi",
    "changes/D4_w2_export_module_boundary.mimi",
    "changes/D5_w2_two_background_tasks.mimi",
    "changes/H1_w3_gain_algorithm.mimi",
    "changes/H2_w3_optional_filter.mimi",
    "changes/H3_w3_shared_snapshot.mimi",
    "changes/S1_w4_ffi_replacement.mimi",
    "changes/S2_w4_timeout_handoff.mimi",
    "changes/S3_w4_endpoint_scope.mimi",
    "changes/S4_w4_transport_split.mimi",
}
ORACLE_MODES = {
    "mimi_stdout",
    "mimi_check_reject",
    "w1_scoped_cli",
    "w4_host_driver",
    "w4_endpoint_scope",
    "blocked_static_negative",
    "not_materialized",
}


class ManifestError(ValueError):
    """The shared fixture manifest is incomplete or inconsistent."""


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ManifestError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def read_json(path: Path, *, label: str) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicate_keys)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ManifestError(f"cannot read {label}: {error}") from error


def safe_relative(value: Any, *, field: str) -> PurePosixPath:
    if not isinstance(value, str) or not value or "\\" in value:
        raise ManifestError(f"{field} must be a non-empty POSIX relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or path.as_posix() != value or any(part in ("", ".", "..") for part in path.parts):
        raise ManifestError(f"{field} escapes its declared root: {value!r}")
    return path


def confined(root: Path, value: Any, *, field: str) -> Path:
    relative = safe_relative(value, field=field)
    path = root.joinpath(*relative.parts)
    try:
        path.resolve(strict=False).relative_to(root.resolve())
    except ValueError as error:
        raise ManifestError(f"{field} escapes its declared root: {value!r}") from error
    return path


def resolve_key(document: Any, key: str, *, field: str) -> Any:
    if key == "$":
        return document
    if key in document if isinstance(document, dict) else False:
        return document[key]
    value = document
    for part in key.strip("/").split("/"):
        if not part or not isinstance(value, dict) or part not in value:
            raise ManifestError(f"{field} key {key!r} does not resolve")
        value = value[part]
    return value


def all_fixture_files() -> dict[str, Path]:
    found: dict[str, Path] = {}
    for path in sorted(FIXTURES.rglob("*")):
        if path.is_symlink():
            relative = path.relative_to(FIXTURES).as_posix()
            raise ManifestError(f"symlinks are not valid fixture inputs: {relative}")
        if path.is_file() and path != MANIFEST_PATH:
            found[path.relative_to(FIXTURES).as_posix()] = path
    return found


def expect_object(value: Any, *, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ManifestError(f"{field} must resolve to a JSON object")
    return value


def require_string_list(value: Any, *, field: str, non_empty: bool = False) -> list[str]:
    if not isinstance(value, list) or (non_empty and not value) or not all(
        isinstance(item, str) and item for item in value
    ):
        qualifier = "non-empty " if non_empty else ""
        raise ManifestError(f"{field} must be a {qualifier}list of non-empty strings")
    return value


def validate_oracle(
    oracle: Any,
    artifacts: dict[str, dict[str, Any]],
    *,
    field: str,
    allow_pair: bool = True,
) -> dict[str, Any]:
    descriptor = expect_object(oracle, field=field)
    required = {"mode", "file", "key", "support_files"}
    allowed = required | {"sha256", "paired_mimi"}
    if not required.issubset(descriptor) or set(descriptor) - allowed:
        raise ManifestError(f"{field} must contain exactly the oracle descriptor fields {sorted(allowed)}")
    mode = descriptor["mode"]
    if mode not in ORACLE_MODES:
        raise ManifestError(f"{field}.mode is unsupported: {mode!r}")
    if descriptor["file"] == "oracle/v02_expected.json":
        raise ManifestError(f"{field} refers to stale probe oracle data")
    oracle_path = safe_relative(descriptor["file"], field=f"{field}.file").as_posix()
    if oracle_path not in artifacts:
        raise ManifestError(f"{field}.file is missing from the artifact inventory: {oracle_path}")
    expected_hash = descriptor.get("sha256")
    if not isinstance(expected_hash, str) or len(expected_hash) != SHA256_LENGTH:
        raise ManifestError(f"{field}.sha256 must bind the oracle file hash")
    if expected_hash != artifacts[oracle_path]["sha256"]:
        raise ManifestError(f"{field}.sha256 disagrees with artifact hash for {oracle_path}")
    if any(ch not in "0123456789abcdef" for ch in expected_hash):
        raise ManifestError(f"{field}.sha256 must be lowercase hexadecimal")
    if artifacts[oracle_path]["role"] != "independent_app_oracle":
        raise ManifestError(f"{field}.file is not tagged as an independent app oracle")
    if not isinstance(descriptor["key"], str):
        raise ManifestError(f"{field}.key must be a string")
    content = read_json(FIXTURES / oracle_path, label=f"oracle {oracle_path}")
    selected = resolve_key(content, descriptor["key"], field=field)
    if mode in {"mimi_stdout", "w4_endpoint_scope"}:
        selected_object = expect_object(selected, field=f"{field} selected value")
        if not isinstance(selected_object.get("stdout"), str) or not isinstance(selected_object.get("files"), dict):
            raise ManifestError(f"{field} selected value must contain stdout and files")
        if not all(isinstance(path, str) and isinstance(contents, str) for path, contents in selected_object["files"].items()):
            raise ManifestError(f"{field} selected file outputs must map strings to strings")
    if mode == "w4_host_driver":
        selected_object = expect_object(selected, field=f"{field} selected value")
        if not {"checksum", "clients", "stop", "exports", "limits"}.issubset(selected_object):
            raise ManifestError(f"{field} does not resolve to the complete W4 host oracle")
    if mode == "mimi_check_reject":
        selected_object = expect_object(selected, field=f"{field} selected value")
        diagnostics = selected_object.get("diagnostic_contains")
        if isinstance(diagnostics, str):
            diagnostics = [diagnostics]
        if not isinstance(diagnostics, list) or not diagnostics or not all(
            isinstance(item, str) and item for item in diagnostics
        ):
            raise ManifestError(f"{field} check-reject oracle needs diagnostic_contains strings")
    if mode == "blocked_static_negative":
        selected_object = expect_object(selected, field=f"{field} selected value")
        if selected_object.get("status") not in {"blocked", "not_materialized"}:
            raise ManifestError(f"{field} must resolve to an explicit static-negative blocker")
        if not isinstance(selected_object.get("expected"), str) or not selected_object["expected"]:
            raise ManifestError(f"{field} static-negative oracle needs an expected-behavior statement")
        if selected_object["status"] == "blocked" and not selected_object.get("blocker"):
            raise ManifestError(f"{field} blocked static-negative oracle needs a blocker reason")
    support_files = descriptor["support_files"]
    if not isinstance(support_files, list) or not all(isinstance(item, str) for item in support_files):
        raise ManifestError(f"{field}.support_files must be a string path list")
    if len(support_files) != len(set(support_files)):
        raise ManifestError(f"{field}.support_files must be a unique path list")
    for raw in support_files:
        relative = safe_relative(raw, field=f"{field}.support_files").as_posix()
        if relative not in artifacts:
            raise ManifestError(f"{field} support file is absent from inventory: {relative}")
    if "paired_mimi" in descriptor:
        if not allow_pair:
            raise ManifestError(f"{field}.paired_mimi nesting is not allowed")
        pair = validate_oracle(
            descriptor["paired_mimi"], artifacts, field=f"{field}.paired_mimi", allow_pair=False
        )
        if pair["mode"] != "mimi_stdout":
            raise ManifestError(f"{field}.paired_mimi must use mimi_stdout")
    return descriptor


def validate_entry_source(entry: dict[str, Any], artifacts: dict[str, dict[str, Any]], *, field: str) -> None:
    source = entry.get("source")
    sources = entry.get("sources")
    if not isinstance(sources, list) or len(sources) != len(set(sources)):
        raise ManifestError(f"{field}.sources must be a unique path list")
    for raw in sources:
        path = safe_relative(raw, field=f"{field}.sources").as_posix()
        if path not in artifacts:
            raise ManifestError(f"{field} source is absent from inventory: {path}")
        if path in LEGACY_PATHS:
            raise ManifestError(f"{field} uses a legacy probe/anchor as an application source: {path}")
    if source is None:
        if sources:
            raise ManifestError(f"{field} has no primary source but declares source files")
        if entry.get("status") != "not_materialized":
            raise ManifestError(f"{field} may omit source only when status=not_materialized")
    else:
        source_path = safe_relative(source, field=f"{field}.source").as_posix()
        if source_path not in artifacts or source_path not in sources:
            raise ManifestError(f"{field}.source must be present in its declared source closure")
        if source_path in LEGACY_PATHS:
            raise ManifestError(f"{field}.source silently substitutes a legacy probe")
    entrypoint = entry.get("entrypoint")
    root = entry.get("root")
    if entrypoint is not None:
        root_path = safe_relative(root, field=f"{field}.root")
        relative_entrypoint = safe_relative(entrypoint, field=f"{field}.entrypoint")
        expected = (root_path / relative_entrypoint).as_posix()
        if source is not None and source != expected:
            raise ManifestError(f"{field}.source must equal root/entrypoint ({expected})")
        if source is not None and expected not in sources:
            raise ManifestError(f"{field}.entrypoint is not in the source closure")


def validate_manifest() -> list[str]:
    manifest = read_json(MANIFEST_PATH, label="V0.2 app manifest")
    manifest = expect_object(manifest, field="manifest")
    required_top = {
        "schema", "contract", "artifacts", "apps", "changes", "negatives", "sequences", "legacy_inputs"
    }
    if set(manifest) != required_top:
        raise ManifestError(f"manifest keys must be exactly {sorted(required_top)}")
    if manifest["schema"] != "mimi-v02-app-manifest-v2":
        raise ManifestError("manifest schema must be mimi-v02-app-manifest-v2")

    contract = expect_object(manifest["contract"], field="contract")
    if set(contract) != {"documents", "accepted", "release_status", "statement"}:
        raise ManifestError("contract has unexpected or missing fields")
    documents = contract.get("documents")
    if not isinstance(documents, list) or not documents:
        raise ManifestError("contract.documents must enumerate the V0.2 requirement sources")
    if [item.get("path") for item in documents if isinstance(item, dict)] != list(CONTRACT_DOCUMENTS):
        raise ManifestError("contract.documents must enumerate the exact V0.2 requirement sources in order")
    for index, item in enumerate(documents):
        item = expect_object(item, field=f"contract.documents[{index}]")
        if set(item) != {"path", "sha256"}:
            raise ManifestError("contract document entries require exactly path and sha256")
        path = confined(ROOT, item["path"], field="contract document path")
        if not path.is_file() or path.is_symlink():
            raise ManifestError(f"contract document is missing: {item['path']}")
        if item["sha256"] != sha256_file(path):
            raise ManifestError(f"contract document hash mismatch: {item['path']}")
    if contract.get("accepted") is not False or contract.get("release_status") != "not_accepted":
        raise ManifestError("the fixture contract must remain explicitly unaccepted")
    if not isinstance(contract.get("statement"), str) or not contract["statement"]:
        raise ManifestError("contract.statement must state the non-acceptance boundary")

    artifacts = expect_object(manifest["artifacts"], field="artifacts")
    actual_files = all_fixture_files()
    missing = sorted(set(actual_files) - set(artifacts))
    extra = sorted(set(artifacts) - set(actual_files))
    if missing or extra:
        parts = []
        if missing:
            parts.append(f"missing manifest artifacts: {missing}")
        if extra:
            parts.append(f"extra manifest artifacts: {extra}")
        raise ManifestError("; ".join(parts))
    for relative, info in artifacts.items():
        info = expect_object(info, field=f"artifacts[{relative!r}]")
        if set(info) != {"sha256", "role"}:
            raise ManifestError(f"artifacts[{relative!r}] needs exactly sha256 and role")
        digest = info["sha256"]
        if not isinstance(digest, str) or len(digest) != SHA256_LENGTH or any(ch not in "0123456789abcdef" for ch in digest):
            raise ManifestError(f"invalid SHA-256 for artifact {relative}")
        actual_hash = sha256_file(actual_files[relative])
        if digest != actual_hash:
            raise ManifestError(f"artifact SHA-256 mismatch: {relative}")
        if not isinstance(info["role"], str) or not info["role"]:
            raise ManifestError(f"artifact role is missing: {relative}")

    apps = manifest["apps"]
    if not isinstance(apps, list) or [entry.get("id") for entry in apps if isinstance(entry, dict)] != list(APP_IDS):
        raise ManifestError(f"apps must enumerate exactly {list(APP_IDS)} in order")
    app_by_id: dict[str, dict[str, Any]] = {}
    for entry in apps:
        entry = expect_object(entry, field="app entry")
        if set(entry) != {
            "id", "root", "entrypoint", "source", "sources", "source_inputs", "oracle", "status",
            "accepted", "blockers", "requirement_ids", "negative_ids",
        }:
            raise ManifestError("app entry has unexpected or missing fields")
        app_id = entry["id"]
        app_by_id[app_id] = entry
        if entry.get("accepted") is not False:
            raise ManifestError(f"{app_id} must not be marked accepted")
        if entry.get("status") not in {"blocked", "unverified", "incomplete"}:
            raise ManifestError(f"{app_id} has invalid app status")
        require_string_list(entry.get("blockers"), field=f"{app_id}.blockers", non_empty=True)
        require_string_list(entry.get("requirement_ids"), field=f"{app_id}.requirement_ids", non_empty=True)
        validate_entry_source(entry, artifacts, field=app_id)
        expected_sources = APP_SOURCE_CLOSURES[app_id]
        if set(entry["sources"]) != expected_sources:
            raise ManifestError(f"{app_id} source closure mismatch: expected {sorted(expected_sources)}")
        input_files = entry.get("source_inputs")
        if not isinstance(input_files, list) or len(input_files) != len(set(input_files)):
            raise ManifestError(f"{app_id}.source_inputs must be a unique path list")
        for raw in input_files:
            path = safe_relative(raw, field=f"{app_id}.source_inputs").as_posix()
            if path not in artifacts or path in LEGACY_PATHS:
                raise ManifestError(f"{app_id} input is missing or legacy: {path}")
        if app_id == "W1":
            expected_inputs = {f"w1/mimi.toml", *(
                path.relative_to(FIXTURES).as_posix()
                for path in (FIXTURES / "w1" / "sample").rglob("*")
                if path.is_file()
            )}
            if set(input_files) != expected_inputs:
                raise ManifestError("W1 must enumerate its package manifest and every fixed sample input")
        elif set(input_files) != set(APP_SOURCE_INPUTS[app_id]):
            raise ManifestError(f"{app_id}.source_inputs is incomplete or has extra inputs")
        if not isinstance(entry.get("negative_ids"), list):
            raise ManifestError(f"{app_id}.negative_ids must be a list")
        if not all(isinstance(item, str) and item for item in entry["negative_ids"]):
            raise ManifestError(f"{app_id}.negative_ids must contain non-empty strings")
        app_oracle = validate_oracle(entry.get("oracle"), artifacts, field=f"{app_id}.oracle")
        if (app_oracle["mode"], app_oracle["file"], app_oracle["key"]) != APP_ORACLES[app_id]:
            raise ManifestError(f"{app_id} oracle does not reference its independent baseline")

    changes = manifest["changes"]
    if not isinstance(changes, list) or [entry.get("id") for entry in changes if isinstance(entry, dict)] != list(CHANGE_IDS):
        raise ManifestError(f"changes must enumerate exactly {list(CHANGE_IDS)} in order")
    change_by_id: dict[str, dict[str, Any]] = {}
    for entry in changes:
        entry = expect_object(entry, field="change entry")
        change_id = entry.get("id")
        expected_fields = {
            "id", "app", "root", "entrypoint", "source", "sources", "source_kind", "anchor", "oracle",
            "status", "accepted", "blockers", "requirement_ids", "negative_ids",
        }
        if change_id in CHANGE_SOURCE_INPUTS:
            expected_fields.add("source_inputs")
        if set(entry) != expected_fields:
            raise ManifestError("change entry has unexpected or missing fields")
        change_id = entry["id"]
        change_by_id[change_id] = entry
        app_id = entry.get("app")
        expected_app = "W1" if change_id.startswith("C") else "W2" if change_id.startswith("D") else "W3" if change_id.startswith("H") else "W4"
        if app_id != expected_app:
            raise ManifestError(f"{change_id} must map to {expected_app}")
        require_string_list(entry.get("requirement_ids"), field=f"{change_id}.requirement_ids", non_empty=True)
        require_string_list(entry.get("blockers"), field=f"{change_id}.blockers", non_empty=True)
        require_string_list(entry.get("negative_ids"), field=f"{change_id}.negative_ids")
        if not isinstance(entry.get("source_kind"), str) or not entry["source_kind"]:
            raise ManifestError(f"{change_id}.source_kind must be a non-empty string")
        expected_source, expected_closure = CHANGE_SOURCE_CLOSURES[change_id]
        change_sources = entry.get("sources")
        if not isinstance(change_sources, list) or entry.get("source") != expected_source or tuple(change_sources) != expected_closure:
            raise ManifestError(f"{change_id} source closure does not match its declared change")
        change_negative_ids = entry.get("negative_ids")
        if not isinstance(change_negative_ids, list) or not all(
            isinstance(item, str) and item for item in change_negative_ids
        ):
            raise ManifestError(f"{change_id}.negative_ids must be a string list")
        if change_id in CHANGE_SOURCE_INPUTS:
            source_inputs = entry.get("source_inputs")
            if not isinstance(source_inputs, list) or tuple(source_inputs) != CHANGE_SOURCE_INPUTS[change_id]:
                raise ManifestError(f"{change_id} package/lock inputs are incomplete or out of order")
            for raw in source_inputs:
                path = safe_relative(raw, field=f"{change_id}.source_inputs").as_posix()
                if path not in artifacts:
                    raise ManifestError(f"{change_id} source input is missing: {path}")
        if entry.get("accepted") is not False or entry.get("status") not in {"blocked", "unverified", "anchor_only", "not_materialized"}:
            raise ManifestError(f"{change_id} must remain unaccepted and explicitly non-passing")
        if not isinstance(entry.get("blockers"), list) or not entry["blockers"]:
            raise ManifestError(f"{change_id} must carry its unresolved status/blocker")
        if entry.get("source") is None:
            if entry.get("status") != "not_materialized" or entry.get("sources") != []:
                raise ManifestError(f"{change_id} with no app source must be not_materialized")
            if entry.get("anchor") not in LEGACY_PATHS:
                raise ManifestError(f"{change_id} missing source needs an explicitly legacy anchor")
        else:
            validate_entry_source(entry, artifacts, field=change_id)
        anchor = entry.get("anchor")
        if anchor is not None:
            anchor_path = safe_relative(anchor, field=f"{change_id}.anchor").as_posix()
            if anchor_path not in artifacts:
                raise ManifestError(f"{change_id} anchor missing from artifact inventory: {anchor_path}")
        change_oracle = validate_oracle(entry.get("oracle"), artifacts, field=f"{change_id}.oracle")
        if (change_oracle["mode"], change_oracle["file"], change_oracle["key"]) != CHANGE_ORACLES[change_id]:
            raise ManifestError(f"{change_id} oracle is not bound to its independent expected behavior")

    negatives = manifest["negatives"]
    if not isinstance(negatives, list):
        raise ManifestError("negatives must be an ordered list")
    negative_by_id: dict[str, dict[str, Any]] = {}
    for entry in negatives:
        entry = expect_object(entry, field="negative entry")
        if set(entry) != {
            "id", "app", "source", "input_files", "oracle", "status", "accepted", "requirement_ids",
            "expected", "blockers",
        }:
            raise ManifestError("negative entry has unexpected or missing fields")
        negative_id = entry.get("id")
        if not isinstance(negative_id, str) or negative_id in negative_by_id:
            raise ManifestError(f"invalid or duplicate negative ID: {negative_id!r}")
        if negative_id not in NEGATIVE_BINDINGS:
            raise ManifestError(f"unknown negative requirement: {negative_id!r}")
        negative_by_id[negative_id] = entry
        if entry.get("app") not in app_by_id:
            raise ManifestError(f"{negative_id} maps to an unknown app")
        expected_source, expected_inputs, expected_oracle = NEGATIVE_BINDINGS[negative_id]
        oracle_descriptor = expect_object(entry.get("oracle"), field=f"{negative_id}.oracle")
        if entry.get("source") != expected_source:
            raise ManifestError(f"{negative_id} source does not match its frozen fixture")
        input_files = entry.get("input_files")
        if not isinstance(input_files, list):
            raise ManifestError(f"{negative_id}.input_files must be a list")
        if tuple(input_files) != expected_inputs:
            raise ManifestError(f"{negative_id} input_files do not match its frozen input set")
        if (oracle_descriptor.get("mode"), oracle_descriptor.get("file"), oracle_descriptor.get("key")) != expected_oracle:
            raise ManifestError(f"{negative_id} oracle does not match its frozen requirement key")
        require_string_list(entry.get("requirement_ids"), field=f"{negative_id}.requirement_ids", non_empty=True)
        require_string_list(entry.get("blockers"), field=f"{negative_id}.blockers", non_empty=True)
        if entry.get("accepted") is not False or entry.get("status") not in {"blocked", "unverified", "not_materialized"}:
            raise ManifestError(f"{negative_id} must remain unaccepted")
        if not isinstance(entry.get("expected"), dict):
            raise ManifestError(f"{negative_id} must have an explicit expected behavior object")
        if not isinstance(entry.get("blockers"), list) or not entry["blockers"]:
            raise ManifestError(f"{negative_id} must carry an explicit validation limitation")
        source = entry.get("source")
        if source is not None:
            source_path = safe_relative(source, field=f"{negative_id}.source").as_posix()
            if source_path not in artifacts or source_path in LEGACY_PATHS:
                raise ManifestError(f"{negative_id} source missing or legacy: {source_path}")
        for raw in input_files:
            input_path = safe_relative(raw, field=f"{negative_id}.input_files").as_posix()
            if input_path not in artifacts:
                raise ManifestError(f"{negative_id} input missing from artifact inventory: {input_path}")
        source_inputs = entry.get("source_inputs", [])
        if not isinstance(source_inputs, list):
            raise ManifestError(f"{negative_id}.source_inputs must be a list")
        for raw in source_inputs:
            input_path = safe_relative(raw, field=f"{negative_id}.source_inputs").as_posix()
            if input_path not in artifacts or input_path in LEGACY_PATHS:
                raise ManifestError(f"{negative_id} source input missing or legacy: {input_path}")
        validate_oracle(entry.get("oracle"), artifacts, field=f"{negative_id}.oracle")
    if set(negative_by_id) != NEGATIVE_IDS or set(NEGATIVE_BINDINGS) != NEGATIVE_IDS:
        raise ManifestError(
            f"negative inventory mismatch: missing={sorted(NEGATIVE_IDS - set(negative_by_id))}, "
            f"extra={sorted(set(negative_by_id) - NEGATIVE_IDS)}"
        )
    for app_id, entry in app_by_id.items():
        listed = entry["negative_ids"]
        if len(listed) != len(set(listed)) or any(item not in negative_by_id for item in listed):
            raise ManifestError(f"{app_id}.negative_ids contains duplicate or unknown IDs")
        if {negative_id for negative_id, negative in negative_by_id.items() if negative["app"] == app_id} != set(listed):
            raise ManifestError(f"{app_id}.negative_ids must enumerate every negative assigned to that app")

    sequences = manifest["sequences"]
    if not isinstance(sequences, list) or [seq.get("id") for seq in sequences if isinstance(seq, dict)] != list(SEQUENCE_STEPS):
        raise ManifestError("sequences must enumerate SEQ-W2 then SEQ-W4 exactly")
    for sequence in sequences:
        sequence = expect_object(sequence, field="sequence")
        if set(sequence) != {"id", "execution", "status", "accepted", "baseline", "steps", "inherits", "blockers"}:
            raise ManifestError("sequence has unexpected or missing fields")
        sequence_id = sequence["id"]
        expected = SEQUENCE_STEPS[sequence_id]
        if sequence.get("execution") != "cumulative_patch_chain":
            raise ManifestError(f"{sequence_id} must use cumulative_patch_chain execution")
        if sequence.get("accepted") is not False or sequence.get("status") != "blocked":
            raise ManifestError(f"{sequence_id} must remain blocked and unaccepted")
        require_string_list(sequence.get("blockers"), field=f"{sequence_id}.blockers", non_empty=True)
        expected_inherits = (
            ["previous source tree", "previous external data", "all historical regressions"]
            if sequence_id == "SEQ-W2"
            else [
                "previous source tree", "offline lock/cache state", "fixed request/oracle corpus",
                "all historical regressions",
            ]
        )
        if sequence.get("inherits") != expected_inherits:
            raise ManifestError(f"{sequence_id}.inherits omits required cumulative state")
        baseline = expect_object(sequence.get("baseline"), field=f"{sequence_id}.baseline")
        if set(baseline) != {
            "fixture", "root", "entrypoint", "status", "accepted", "source_manifest", "cumulative_regressions",
            "regression_targets", "acceptance_status", "blockers",
        }:
            raise ManifestError(f"{sequence_id}.baseline has unexpected or missing fields")
        if baseline.get("fixture") != expected[0] or baseline.get("status") not in {"ready", "blocked"}:
            raise ManifestError(f"{sequence_id} baseline fixture/status is invalid")
        root_text = safe_relative(baseline.get("root"), field=f"{sequence_id}.baseline.root").as_posix()
        root = confined(FIXTURES, root_text, field=f"{sequence_id}.baseline.root")
        entrypoint = safe_relative(baseline.get("entrypoint"), field=f"{sequence_id}.baseline.entrypoint").as_posix()
        source_manifest = baseline.get("source_manifest")
        if not isinstance(source_manifest, list) or not source_manifest:
            raise ManifestError(f"{sequence_id}.baseline.source_manifest must be non-empty")
        source_paths: set[str] = set()
        for item in source_manifest:
            item = expect_object(item, field=f"{sequence_id}.baseline.source_manifest item")
            if set(item) != {"path", "sha256", "role"}:
                raise ManifestError(f"{sequence_id} source_manifest items need path, sha256, role")
            relative = safe_relative(item["path"], field=f"{sequence_id}.baseline.source_manifest.path").as_posix()
            if relative in source_paths:
                raise ManifestError(f"{sequence_id} source_manifest repeats {relative}")
            source_paths.add(relative)
            path = confined(root, relative, field=f"{sequence_id} source input")
            global_path = f"{root_text}/{relative}"
            if not path.is_file() or global_path not in artifacts:
                raise ManifestError(f"{sequence_id} source input is missing: {global_path}")
            if item["sha256"] != artifacts[global_path]["sha256"] or item["sha256"] != sha256_file(path):
                raise ManifestError(f"{sequence_id} source input hash mismatch: {global_path}")
            if not isinstance(item["role"], str) or not item["role"]:
                raise ManifestError(f"{sequence_id} source input role is missing: {global_path}")
        if entrypoint not in source_paths:
            raise ManifestError(f"{sequence_id} baseline entrypoint is not in source_manifest")
        if baseline.get("acceptance_status") != "blocked" or not baseline.get("blockers"):
            raise ManifestError(f"{sequence_id} baseline must remain explicitly blocked for acceptance")
        if baseline.get("accepted") is not False:
            raise ManifestError(f"{sequence_id} baseline cannot be accepted")
        require_string_list(baseline.get("blockers"), field=f"{sequence_id}.baseline.blockers", non_empty=True)
        if baseline.get("regression_targets") != []:
            raise ManifestError(f"{sequence_id} baseline targets are not yet wired and must stay empty")
        if sequence_id == "SEQ-W2" and source_paths != {"w2_document_core.mimi"}:
            raise ManifestError("SEQ-W2 baseline must contain only the W2 application source")
        if sequence_id == "SEQ-W4" and source_paths != {
            "baseline/mimi.toml", "baseline/native/w4_trusted.c", "baseline/src/exports.mimi",
            "baseline/src/main.mimi", "baseline/src/protocol.mimi", "baseline/src/server.mimi"
        }:
            raise ManifestError("SEQ-W4 baseline source_manifest is incomplete or includes non-baseline anchors")

        steps = sequence.get("steps")
        if not isinstance(steps, list) or [step.get("fixture") for step in steps if isinstance(step, dict)] != list(expected[1:]):
            raise ManifestError(f"{sequence_id} step order is incomplete or changed")
        for index, step in enumerate(steps, start=1):
            step = expect_object(step, field=f"{sequence_id} step")
            if set(step) != {
                "fixture", "entrypoint", "status", "accepted", "patch_path", "patch", "anchor", "oracle",
                "cumulative_regressions", "regression_targets", "acceptance_status", "blockers",
            }:
                raise ManifestError(f"{sequence_id} step has unexpected or missing fields")
            fixture_id = expected[index]
            if step.get("status") not in {"ready", "blocked"}:
                raise ManifestError(f"{sequence_id}/{fixture_id} has invalid checkpoint status")
            if step.get("accepted") is not False:
                raise ManifestError(f"{sequence_id}/{fixture_id} cannot be accepted in this manifest")
            if step.get("cumulative_regressions") != list(expected[: index + 1]):
                raise ManifestError(f"{sequence_id}/{fixture_id} cumulative_regressions must be the exact prefix")
            if not isinstance(step.get("regression_targets"), list):
                raise ManifestError(f"{sequence_id}/{fixture_id} regression_targets must be a list")
            target_ids = [target.get("fixture") for target in step["regression_targets"] if isinstance(target, dict)]
            if target_ids and target_ids != list(expected[: index + 1]):
                raise ManifestError(f"{sequence_id}/{fixture_id} targets must map the full prefix in order")
            if not target_ids and (step.get("acceptance_status") != "blocked" or not step.get("blockers")):
                raise ManifestError(f"{sequence_id}/{fixture_id} without targets must be explicitly blocked for acceptance")
            anchor = safe_relative(step.get("anchor"), field=f"{sequence_id}/{fixture_id}.anchor").as_posix()
            if anchor not in artifacts:
                raise ManifestError(f"{sequence_id}/{fixture_id} anchor is missing: {anchor}")
            if sequence_id == "SEQ-W2":
                expected_entrypoint = "w2_document_core.mimi"
            elif fixture_id == "S4":
                expected_entrypoint = "offline-release/src/main.mimi"
            else:
                expected_entrypoint = "baseline/src/main.mimi"
            if step.get("entrypoint") != expected_entrypoint:
                raise ManifestError(f"{sequence_id}/{fixture_id} entrypoint must be {expected_entrypoint}")
            patch_path = safe_relative(step.get("patch_path"), field=f"{sequence_id}/{fixture_id}.patch_path").as_posix()
            if patch_path != PATCH_PATHS[fixture_id]:
                raise ManifestError(f"{sequence_id}/{fixture_id} patch_path must be {PATCH_PATHS[fixture_id]}")
            patch = step.get("patch")
            if patch is None:
                if patch_path in artifacts or step.get("status") != "blocked":
                    raise ManifestError(f"{sequence_id}/{fixture_id} patch is missing but the artifact/status disagrees")
            else:
                patch = expect_object(patch, field=f"{sequence_id}/{fixture_id}.patch")
                if set(patch) != {"path", "sha256", "format"}:
                    raise ManifestError(f"{sequence_id}/{fixture_id}.patch needs path, sha256, format")
                if patch["path"] != patch_path or patch["format"] != "unified_diff":
                    raise ManifestError(f"{sequence_id}/{fixture_id} patch path/format is invalid")
                if patch_path not in artifacts or patch["sha256"] != artifacts[patch_path]["sha256"]:
                    raise ManifestError(f"{sequence_id}/{fixture_id} patch hash is not bound to its artifact")
                if step.get("status") != "ready":
                    raise ManifestError(f"{sequence_id}/{fixture_id} has a real patch but is not marked source-ready")
            require_string_list(step.get("blockers"), field=f"{sequence_id}/{fixture_id}.blockers", non_empty=True)
            step_oracle = validate_oracle(step.get("oracle"), artifacts, field=f"{sequence_id}/{fixture_id}.oracle")
            if sequence_id == "SEQ-W2":
                expected_oracle = ("mimi_stdout", "oracle/w2_expected.json", f"sequence_regressions/{fixture_id}")
            elif fixture_id == "S3":
                expected_oracle = ("w4_endpoint_scope", "oracle/w4_changes_expected.json", "S3/endpoint_scope")
            else:
                expected_oracle = ("w4_host_driver", "w4/oracle/w4_expected.json", "$")
            if (step_oracle["mode"], step_oracle["file"], step_oracle["key"]) != expected_oracle:
                raise ManifestError(f"{sequence_id}/{fixture_id} oracle is not bound to its independent checkpoint expectation")
            for target in step["regression_targets"]:
                target = expect_object(target, field=f"{sequence_id}/{fixture_id} regression target")
                if set(target) != {"fixture", "entrypoint", "oracle"}:
                    raise ManifestError(f"{sequence_id}/{fixture_id} regression target has unexpected fields")
                safe_relative(target.get("entrypoint"), field="regression target entrypoint")
                if target.get("fixture") not in expected[: index + 1]:
                    raise ManifestError(f"{sequence_id}/{fixture_id} target is outside the cumulative prefix")
                validate_oracle(target.get("oracle"), artifacts, field="regression target oracle")

    legacy_inputs = manifest["legacy_inputs"]
    if not isinstance(legacy_inputs, list):
        raise ManifestError("legacy_inputs must be a list")
    legacy_map: dict[str, dict[str, Any]] = {}
    for entry in legacy_inputs:
        entry = expect_object(entry, field="legacy input")
        if set(entry) != {"path", "accepted", "reason"}:
            raise ManifestError("legacy input has unexpected or missing fields")
        path = safe_relative(entry.get("path"), field="legacy input path").as_posix()
        if path in legacy_map:
            raise ManifestError(f"duplicate legacy input path: {path}")
        legacy_map[path] = entry
        if entry.get("accepted") is not False or not isinstance(entry.get("reason"), str) or not entry["reason"]:
            raise ManifestError(f"legacy input {path} must be explicitly excluded from acceptance")
        if path not in artifacts:
            raise ManifestError(f"legacy input is missing from artifact inventory: {path}")
    if set(legacy_map) != LEGACY_PATHS:
        raise ManifestError(
            f"legacy input inventory mismatch: missing={sorted(LEGACY_PATHS - set(legacy_map))}, "
            f"extra={sorted(set(legacy_map) - LEGACY_PATHS)}"
        )
    for path in LEGACY_PATHS:
        if path not in artifacts:
            raise ManifestError(f"required legacy probe/anchor is absent: {path}")

    return [
        f"validated {len(artifacts)} fixture artifacts and SHA-256 bindings",
        f"validated {len(apps)} W1-W4 applications, {len(changes)} changes, and {len(negatives)} negative requirements",
        "validated SEQ-W2/SEQ-W4 order, source manifests, patch hashes, and fail-closed blockers",
        "validated legacy probes/oracles are inventory-only and excluded from app acceptance",
    ]


def main() -> int:
    try:
        messages = validate_manifest()
    except ManifestError as error:
        print(f"V0.2 manifest error: {error}", file=sys.stderr)
        return 1
    for message in messages:
        print(f"OK: {message}")
    print("Acceptance status: not accepted (this check validates fixture structure only)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
