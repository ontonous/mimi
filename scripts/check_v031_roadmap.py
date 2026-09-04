#!/usr/bin/env python3
"""Validate the v0.31 roadmap against normative requirement IDs."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REQUIREMENTS = ROOT / "docs/language-requirements.toml"
KINDS = {
    "baseline",
    "implementation",
    "stabilization",
    "evidence",
    "audit",
    "debug",
    "rc",
    "spike",
    "deferred",
    "soundness",
    "completeness",
}
NO_FEATURE_KINDS = {
    "stabilization",
    "audit",
    "debug",
    "rc",
    "spike",
    "deferred",
    "soundness",
    "completeness",
}
# v0.31 归档快照之后新增的 requirement，无法回填 0.31 里程碑归属。
# 每一项必须登记引入出处；新增 post-0.31 requirement 时在此追加。
POST_V0_31_REQUIREMENTS = {
    # 0.1.8（0.38.122）：Flow `flow_drop` 显式释放 + EPOCH_ERR_STALE
    "FLOW-EPOCH-DROP-001",
}


def main() -> int:
    errors: list[str] = []
    # v0.31 目录已归档至 devdocs/archive/v0.31/（AGENTS.md §13 权威路径）；
    # 旧路径保留为历史检出的回退。
    roadmap_path = next(
        (
            candidate
            for candidate in (
                ROOT / "devdocs/archive/v0.31/roadmap.toml",
                ROOT / "devdocs/v0.31/roadmap.toml",
            )
            if candidate.is_file()
        ),
        None,
    )
    if roadmap_path is None:
        if not (ROOT / "devdocs").is_dir():
            # devdocs/ 已于 2026-08-31 移出 git 跟踪（internal-only）；公共检出无此
            # 目录，跳过而非红闸。本地检出（devdocs 在场）仍全量执法。
            print(
                "v0.31 roadmap check skipped: "
                "devdocs/ is internal-only (untracked) on this checkout"
            )
            return 0
        print(
            "error: missing devdocs/archive/v0.31/roadmap.toml",
            file=sys.stderr,
        )
        return 1
    with roadmap_path.open("rb") as stream:
        roadmap = tomllib.load(stream)
    with REQUIREMENTS.open("rb") as stream:
        requirement_doc = tomllib.load(stream)

    requirement_ids = {item["id"] for item in requirement_doc.get("requirement", [])}
    milestones = roadmap.get("milestone", [])
    first = roadmap.get("first")
    last = roadmap.get("last")
    expected_versions = [f"0.31.{index}" for index in range(first, last + 1)]
    actual_versions = [item.get("version") for item in milestones]
    if actual_versions != expected_versions:
        errors.append("milestone versions must be contiguous and ordered from first to last")

    assigned: set[str] = set()
    for item in milestones:
        version = item.get("version", "<unknown>")
        kind = item.get("kind")
        requirements = item.get("requirements", [])
        if kind not in KINDS:
            errors.append(f"{version}: invalid kind {kind!r}")
        if not isinstance(item.get("title"), str) or not item["title"].strip():
            errors.append(f"{version}: non-empty title required")
        if not isinstance(requirements, list):
            errors.append(f"{version}: requirements must be a list")
            continue
        unknown = set(requirements) - requirement_ids
        if unknown:
            errors.append(f"{version}: unknown requirements {sorted(unknown)}")
        if kind in NO_FEATURE_KINDS and requirements:
            errors.append(f"{version}: {kind} milestone cannot introduce requirements")
        assigned.update(requirements)

    missing = requirement_ids - assigned - POST_V0_31_REQUIREMENTS
    if missing:
        errors.append(f"requirements without a v0.31 milestone: {sorted(missing)}")

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"v0.31 roadmap valid: {len(milestones)} milestones, {len(assigned)} requirements")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
