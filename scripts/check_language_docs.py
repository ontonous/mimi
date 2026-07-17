#!/usr/bin/env python3
"""Validate Mimi language specification manifests without third-party deps."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "docs/language-spec.md"
REQUIREMENTS = ROOT / "docs/language-requirements.toml"
SUPPORT = ROOT / "docs/language-support.toml"

TARGETS = {"stable", "experimental", "reserved", "removed"}
MATURITY = {"unsupported", "partial", "complete", "not_applicable"}
GATES = {"static", "trace", "verifier", "component", "tooling", "migration"}
PROFILES = {
    "mimi-resolved-ir-1": "docs/spec/resolved-ir.md",
    "mimi-flow-turn-1": "docs/spec/transition-turn.md",
    "mimi-semantic-trace-1": "docs/spec/semantic-trace.md",
    "mimi-verified-core-1": "docs/spec/verified-core-1.md",
    "mimi-native-abi-1": "docs/spec/native-abi-1.md",
    "mimi-wire-schema-1": "docs/spec/wire-schema-1.md",
}
DIMENSIONS = {
    "implementation",
    "parse",
    "check",
    "resolved_ir",
    "interp",
    "codegen",
    "runtime",
    "verify",
    "fmt",
    "lsp",
}


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def main() -> int:
    errors: list[str] = []
    spec_text = SPEC.read_text(encoding="utf-8")

    with REQUIREMENTS.open("rb") as stream:
        requirements_doc = tomllib.load(stream)
    with SUPPORT.open("rb") as stream:
        support_doc = tomllib.load(stream)

    requirements = requirements_doc.get("requirement", [])
    supports = support_doc.get("support", [])
    requirement_ids: set[str] = set()
    requirement_map_match = re.search(
        r"^### Normative Requirement Map\s*$\n(?P<body>.*?)(?=^---\s*$)",
        spec_text,
        re.MULTILINE | re.DOTALL,
    )
    requirement_map_ids = (
        set(re.findall(r"`([A-Z][A-Z0-9-]+-\d{3})`", requirement_map_match.group("body")))
        if requirement_map_match
        else set()
    )
    if requirement_map_match is None:
        fail(errors, "language-spec.md is missing the normative requirement map")
    section_ids = {
        match.group(1)
        for match in re.finditer(r"^#+\s+(\d+(?:\.\d+)*)\.?(?:\s|$)", spec_text, re.MULTILINE)
    }

    for index, item in enumerate(requirements, 1):
        item_id = item.get("id")
        prefix = f"requirement[{index}]"
        if not isinstance(item_id, str) or not re.fullmatch(r"[A-Z][A-Z0-9-]+-\d{3}", item_id):
            fail(errors, f"{prefix}: invalid id {item_id!r}")
            continue
        if item_id in requirement_ids:
            fail(errors, f"{prefix}: duplicate id {item_id}")
        requirement_ids.add(item_id)
        if f"`{item_id}`" not in spec_text:
            fail(errors, f"{item_id}: not referenced by the normative specification")
        if item.get("target") not in TARGETS:
            fail(errors, f"{item_id}: invalid target {item.get('target')!r}")
        if not item.get("gate"):
            fail(errors, f"{item_id}: at least one gate is required")
        elif not isinstance(item["gate"], list) or any(
            gate not in GATES for gate in item["gate"]
        ):
            fail(errors, f"{item_id}: invalid gate list {item.get('gate')!r}")
        profiles = item.get("profile", [])
        if not isinstance(profiles, list) or any(profile not in PROFILES for profile in profiles):
            fail(errors, f"{item_id}: invalid profile list {profiles!r}")
        section = item.get("spec")
        if not isinstance(section, str) or section not in section_ids:
            fail(errors, f"{item_id}: missing spec section {section!r}")

    support_ids: set[str] = set()
    for index, item in enumerate(supports, 1):
        item_id = item.get("requirement")
        prefix = f"support[{index}]"
        if item_id not in requirement_ids:
            fail(errors, f"{prefix}: unknown requirement {item_id!r}")
        if item_id in support_ids:
            fail(errors, f"{prefix}: duplicate support for {item_id}")
        support_ids.add(item_id)
        for dimension in DIMENSIONS:
            value = item.get(dimension)
            if value not in MATURITY:
                fail(errors, f"{item_id}: invalid {dimension} value {value!r}")
        implementation = item.get("implementation")
        tool_values = [item.get(dimension) for dimension in DIMENSIONS - {"implementation"}]
        if implementation == "complete" and any(
            value not in {"complete", "not_applicable"} for value in tool_values
        ):
            fail(errors, f"{item_id}: complete implementation has incomplete tool dimensions")
        if implementation == "unsupported" and any(value == "complete" for value in tool_values):
            fail(errors, f"{item_id}: unsupported implementation has a complete tool dimension")
        if not isinstance(item.get("probe"), str) or not item["probe"].strip():
            fail(errors, f"{item_id}: non-empty probe is required")
        if not isinstance(item.get("evidence"), str) or not item["evidence"].strip():
            fail(errors, f"{item_id}: non-empty evidence is required")

    missing_support = requirement_ids - support_ids
    if missing_support:
        fail(errors, f"requirements without support entries: {sorted(missing_support)}")
    if requirement_map_ids != requirement_ids:
        fail(
            errors,
            "normative requirement map differs from requirements manifest: "
            f"missing={sorted(requirement_ids - requirement_map_ids)}, "
            f"unknown={sorted(requirement_map_ids - requirement_ids)}",
        )

    forbidden = re.findall(r"\[(?:not-yet-implemented|partial)\]", spec_text)
    if forbidden:
        fail(errors, "language-spec.md contains implementation-progress status tags")
    if "Implementation version" in spec_text or "Completion Checklist" in spec_text:
        fail(errors, "language-spec.md contains non-normative implementation progress")

    for profile, relative_path in PROFILES.items():
        profile_path = ROOT / relative_path
        if not profile_path.is_file():
            fail(errors, f"missing normative profile file: {relative_path}")
            continue
        profile_text = profile_path.read_text(encoding="utf-8")
        if profile not in profile_text or relative_path not in spec_text:
            fail(errors, f"profile {profile} is not bound to its file and main specification")

    appendix_text = (ROOT / "docs/ast-appendix.md").read_text(encoding="utf-8")
    if re.search(r"\[(?:stable|experimental|not-yet-implemented|partial)\]", appendix_text):
        fail(errors, "ast-appendix.md mixes target-status tags into implementation evidence")
    if re.search(r"\|[^\n|]*\b(?:stable|experimental|reserved|removed)\b[^\n|]*\|", appendix_text):
        fail(errors, "ast-appendix.md contains target-status vocabulary in a table cell")

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"language docs valid: {len(requirement_ids)} requirements, "
        f"{len(support_ids)} support entries"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
