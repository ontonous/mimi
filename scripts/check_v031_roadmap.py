#!/usr/bin/env python3
"""Validate the v0.31 roadmap against normative requirement IDs."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ROADMAP = ROOT / "devdocs/v0.31/roadmap.toml"
REQUIREMENTS = ROOT / "docs/language-requirements.toml"
KINDS = {"baseline", "implementation", "stabilization", "evidence", "audit", "debug", "rc"}
NO_FEATURE_KINDS = {"stabilization", "audit", "debug", "rc"}


def main() -> int:
    errors: list[str] = []
    with ROADMAP.open("rb") as stream:
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

    missing = requirement_ids - assigned
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
