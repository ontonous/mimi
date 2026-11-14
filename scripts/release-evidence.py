#!/usr/bin/env python3
"""CLI wrapper for the source-bound V0.2 release evidence collector."""

import sys
from pathlib import Path

# Running a script by path does not put ``scripts`` on sys.path on every Python
# launcher, so make the import explicit and deterministic.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from release_evidence import cli

if __name__ == "__main__":
    raise SystemExit(cli())
