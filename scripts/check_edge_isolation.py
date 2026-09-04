#!/usr/bin/env python3
"""Edge isolation gate (0.36.0 Phase 0).

Enforces the edge-decoupling discipline from philosophy-anchor.md §2:
the core gate path (ci.yml lint + test jobs) must have ZERO edge dependencies.

Checks:
  1. Manifest sanity  — every edge item has a valid unique id + marker, and
     `core_dep` is false (edge items must never be a hard dependency of the
     core gate path).
  2. ci.yml isolation — the core gate commands must not reference any edge
     marker (a literal substring match catches wiring an edge test into the
     core path).
  3. Registration     — every `EDGE-GATE:<marker>` tag in src/ and tests/ must
     reference a registered marker (no unregistered edge tags).
  4. Ignored-edge     — an edge-tagged test must be `#[ignore]`d. The sanctioned
     form is `#[ignore = "EDGE-GATE:<marker>: <reason>"]`; an `EDGE-GATE:` tag
     on a non-ignore line that is preceded (within a 6-line window) by `#[test]`
     without an intervening `#[ignore` is a hard error.

Gate-marker convention (devdocs/v0.36/edge-inventory.toml):
  - test code:  `#[ignore = "EDGE-GATE:<marker>: <reason>"]`  (isolated, advisory)
  - source code: `// EDGE-GATE:<marker>`                      (tracking tag)
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "devdocs/v0.36/edge-inventory.toml"
CI = ROOT / ".github/workflows/ci.yml"

MARKER_RE = re.compile(r"EDGE-GATE:([a-z][a-z0-9-]+)")
# Core invariant gates (ci.yml test job) — the tests whose pass/fail is the
# break arbiter. Edge items must never be a hard dependency of these.
CORE_GATE_FILTERS = ("dual_", "typecheck::", "ffi_", "codegen_e2e", "v1_2_verification")


def fail(errors: list[str], msg: str) -> None:
    errors.append(msg)


def scan_rust_files() -> list[tuple[str, str]]:
    """Return [(relpath, text)] for all Rust sources under src/ and tests/."""
    out: list[tuple[str, str]] = []
    for pattern in ("src/**/*.rs", "tests/**/*.rs"):
        for path in sorted(ROOT.glob(pattern)):
            out.append((str(path.relative_to(ROOT)), path.read_text(encoding="utf-8")))
    return out


def main() -> int:
    errors: list[str] = []

    if not INVENTORY.is_file():
        if not (ROOT / "devdocs").is_dir():
            # devdocs/ 已于 2026-08-31 移出 git 跟踪（internal-only）；公共检出无
            # edge inventory，跳过而非红闸。本地检出（devdocs 在场）仍全量执法。
            print(
                "edge isolation check skipped: "
                "devdocs/ is internal-only (untracked) on this checkout"
            )
            return 0
        print(f"error: missing {INVENTORY.relative_to(ROOT)}", file=sys.stderr)
        return 1

    # 1) Manifest sanity.
    with INVENTORY.open("rb") as stream:
        inventory = tomllib.load(stream)
    edges = inventory.get("edge", [])
    if not edges:
        fail(errors, "edge inventory is empty (devdocs/v0.36/edge-inventory.toml)")
    markers: dict[str, str] = {}
    for edge in edges:
        eid = edge.get("id")
        marker = edge.get("marker")
        if not eid or not re.fullmatch(r"EDGE-\d{2}", eid):
            fail(errors, f"edge item has invalid id: {edge!r}")
            continue
        if not marker or not re.fullmatch(r"[a-z][a-z0-9-]+", marker):
            fail(errors, f"{eid}: invalid marker {marker!r}")
            continue
        if edge.get("core_dep") is not False:
            fail(errors, f"{eid}: core_dep must be false (edge item is a core-gate dependency)")
        if marker in markers:
            fail(errors, f"duplicate marker {marker!r} ({markers[marker]} vs {eid})")
        markers[marker] = eid

    # 2) ci.yml core gate path must not reference any edge marker.
    if CI.is_file():
        ci_text = CI.read_text(encoding="utf-8")
        for marker in markers:
            if marker in ci_text:
                fail(
                    errors,
                    f"ci.yml references edge marker {marker!r} "
                    f"({markers[marker]}): edge dependency in core gate path",
                )
        for filt in CORE_GATE_FILTERS:
            if filt not in ci_text:
                fail(errors, f"ci.yml is missing core invariant gate filter {filt!r}")
    else:
        fail(errors, "ci.yml not found")

    # 3) + 4) Registration + ignored-edge discipline over Rust sources.
    unknown: set[str] = set()
    for rel, text in scan_rust_files():
        lines = text.splitlines()
        for idx, line in enumerate(lines):
            for match in MARKER_RE.finditer(line):
                token = match.group(1)
                if token not in markers:
                    unknown.add(token)
                    continue
                # Sanctioned isolated form: the marker sits inside an #[ignore].
                if "#[ignore" in line:
                    continue
                # Look back up to 6 lines for a #[test] without #[ignore].
                window = lines[max(0, idx - 6) : idx]
                has_test = any("#[test]" in w for w in window)
                has_ignore = any("#[ignore" in w for w in window)
                if has_test and not has_ignore:
                    fail(
                        errors,
                        f"{rel}:{idx + 1}: edge tag EDGE-GATE:{token} in a "
                        f"non-ignored #[test] (use #[ignore = \"EDGE-GATE:{token}: <reason>\"])",
                    )

    for token in sorted(unknown):
        fail(errors, f"unregistered edge tag EDGE-GATE:{token} (add it to edge-inventory.toml)")

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"edge isolation valid: {len(edges)} edge items registered, "
        f"core gate path has zero edge dependencies"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
