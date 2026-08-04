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
PRE_1_0 = ROOT / "devdocs/pre-1.0"
GOLDEN_SYNTAX = ROOT / "devdocs/v0.34/golden/syntax-reference.golden.md"
SYNTAX_REFERENCE = ROOT / "docs/syntax-reference.md"

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


# ── Semantic freshness checks (0.34.33) ─────────────────────────────────
# Line-level probes that catch stale references to removed/demoted/promoted
# syntax across the normative doc family. A hit is exempt when its line
# already carries a removal/verdict marker (version-tagged history allowed).
# Regression pins added after the 0.34 doc-sync audit:
#   become/stay removed 0.34.11 (ADR-001), do removed 0.34.27,
#   math is a STABLE verifier channel, multi-target is STABLE (0.34.15-16).

REMOVED_MARKERS = re.compile(
    r"removed|removal|delete[d]?|executed|migrated|abolished|repealed|rescinded|superseded"
    r"|移除|删除|已删|废止|撤销|纠正|修正|取代"
    r"|0\.34\.11|0\.34\.27|ADR-001\b|✅|→|sole|唯一|was:",
    re.IGNORECASE,
)
STABLE_MARKERS = re.compile(
    r"stable|0\.34\.15|0\.34\.28|ADR-002\b|升入|移入|纠正|superseded|rescinded"
    r"|不是 experimental|verifier|✅",
    re.IGNORECASE,
)
BECOME_STAY_PATTERN = re.compile(r"`become`|`stay`|become/stay|become, stay")
DO_PATTERN = re.compile(r"\bdo\b\s*['\"`]?\s*['\"`]?\s*\{|\bStmt::Do\b")
MULTI_TARGET_PATTERN = re.compile(r"multi[- ]?target", re.IGNORECASE)


def check_semantic_freshness(errors: list[str]) -> None:
    scanned: list[tuple[str, str]] = []
    doc_paths = [SPEC, REQUIREMENTS, SUPPORT, SYNTAX_REFERENCE, GOLDEN_SYNTAX]
    doc_paths.extend(sorted(PRE_1_0.glob("*.md")))
    # Extended surface (0.34.33): flagship READMEs, normative spec profiles,
    # implementation appendix.
    doc_paths.extend([ROOT / "README.md", ROOT / "README.zh.md", ROOT / "docs/ast-appendix.md"])
    doc_paths.extend(sorted((ROOT / "docs/spec").glob("*.md")))
    for path in doc_paths:
        if path.is_file():
            scanned.append((str(path.relative_to(ROOT)), path.read_text(encoding="utf-8")))

    for rel, text in scanned:
        for lineno, line in enumerate(text.splitlines(), 1):
            where = f"{rel}:{lineno}"
            if BECOME_STAY_PATTERN.search(line) and not REMOVED_MARKERS.search(line):
                fail(
                    errors,
                    f"{where}: become/stay referenced without a removal marker "
                    "(ADR-001 0.34.11 removed them; sole terminal is `return State {}`)",
                )
            if DO_PATTERN.search(line) and not REMOVED_MARKERS.search(line):
                fail(
                    errors,
                    f"{where}: `do` block referenced without a removal marker "
                    "(v0.34.27 removed the do wrapper; keywords 81→80)",
                )
            if (
                MULTI_TARGET_PATTERN.search(line)
                and re.search(r"experimental", line, re.IGNORECASE)
                and not STABLE_MARKERS.search(line)
            ):
                fail(
                    errors,
                    f"{where}: multi-target described as experimental without a stable "
                    "marker (0.34.15-16 shipped the stable tagged-union ABI, ADR-002)",
                )
            if (
                re.search(r"\bmath\b", line, re.IGNORECASE)
                and "[removed]" in line.lower()
                and not STABLE_MARKERS.search(line)
            ):
                fail(
                    errors,
                    f"{where}: math tagged [removed] without a stable marker "
                    "(0.34.28 verdict: math is a stable verifier channel)",
                )

    # Structural pins on the normative spec checklist (spec §6.12).
    spec_text = ""
    for rel, text in scanned:
        if rel == str(SPEC.relative_to(ROOT)):
            spec_text = text
    stable_match = re.search(
        r"#### Stable targets\s*\n(?P<body>.*?)(?=^#### )",
        spec_text,
        re.MULTILINE | re.DOTALL,
    )
    if stable_match and not re.search(r"multi[- ]?target", stable_match.group("body"), re.IGNORECASE):
        fail(errors, "language-spec.md §6.12 Stable targets is missing multi-target (stable since 0.34.15-16)")
    removed_match = re.search(
        r"#### Removed / Migrated\s*\n(?P<body>.*?)(?=^### |^## |\Z)",
        spec_text,
        re.MULTILINE | re.DOTALL,
    )
    if removed_match:
        for bullet in re.findall(r"^- .*$", removed_match.group("body"), re.MULTILINE):
            if re.search(r"\bmath\b", bullet, re.IGNORECASE):
                fail(errors, "language-spec.md §6.12 Removed list must not contain math (it is stable)")

    # Keyword-count drift pin: docs must agree with the golden EBNF count.
    counts: dict[str, int] = {}
    for rel, text in scanned:
        if rel in {str(SYNTAX_REFERENCE.relative_to(ROOT)), str(GOLDEN_SYNTAX.relative_to(ROOT))}:
            match = re.search(r"当前\s*\**(\d+)\s*个\**\s*`=> TokenKind`", text)
            if match:
                counts[rel] = int(match.group(1))
    if len(counts) == 2 and len(set(counts.values())) > 1:
        fail(errors, f"keyword count drift between golden and docs: {counts}")

    # Manifest pin: multi-target requirement is stable (0.34.28 verdict).
    with REQUIREMENTS.open("rb") as stream:
        req_doc = tomllib.load(stream)
    for item in req_doc.get("requirement", []):
        if item.get("id") == "FLOW-MULTI-001" and item.get("target") != "stable":
            fail(errors, f"FLOW-MULTI-001 target must be stable, got {item.get('target')!r}")


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

    check_semantic_freshness(errors)

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"language docs valid: {len(requirement_ids)} requirements, "
        f"{len(support_ids)} support entries, semantic freshness checks passed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
