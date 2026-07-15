#!/usr/bin/env python3
"""Run the real-world Mimi test suite through interpreter and codegen."""

import subprocess
import sys
from pathlib import Path

import os

ROOT = Path(__file__).resolve().parent
REPO_ROOT = ROOT.parent.parent
MIMI = REPO_ROOT / "target" / "release" / "mimi"
ENV = os.environ.copy()
ENV["LLVM_SYS_180_PREFIX"] = "/tmp/llvm-wrapper"

# Paths are relative to tests/real_world. Add entries only for complete
# programs intentionally unsupported by codegen, not individual weak features.
INTERPRETER_ONLY = frozenset()


def run_one(path: Path):
    rel = path.relative_to(ROOT)
    requires_codegen = rel.as_posix() not in INTERPRETER_ONLY
    # Interpreter
    r = subprocess.run(
        [str(MIMI), "run", str(path)],
        cwd=REPO_ROOT,
        env=ENV,
        capture_output=True,
        text=True,
        timeout=60,
    )
    interp_ok = r.returncode == 0
    interp_out = r.stdout + r.stderr

    cg_ok = None
    cg_out = ""
    exe_ok = None
    exe_out = ""
    exe = REPO_ROOT / path.stem
    if requires_codegen:
        # mimi build defaults to ./<stem> (CWD), not next to the source.
        # Remove stale output so a previous binary cannot mask a failed build.
        try:
            exe.unlink()
        except FileNotFoundError:
            pass

        c = subprocess.run(
            [str(MIMI), "build", str(path)],
            cwd=REPO_ROOT,
            env=ENV,
            capture_output=True,
            text=True,
            timeout=120,
        )
        cg_ok = c.returncode == 0
        cg_out = c.stdout + c.stderr

        if cg_ok:
            if not exe.exists():
                exe_ok = False
                exe_out = "mimi build succeeded but produced no executable"
            else:
                e = subprocess.run(
                    [str(exe)],
                    cwd=REPO_ROOT,
                    env=ENV,
                    capture_output=True,
                    text=True,
                    timeout=60,
                )
                exe_ok = e.returncode == 0
                exe_out = e.stdout + e.stderr
                try:
                    exe.unlink()
                except FileNotFoundError:
                    pass

    return {
        "name": str(rel),
        "requires_codegen": requires_codegen,
        "interp_ok": interp_ok,
        "interp_out": interp_out,
        "cg_ok": cg_ok,
        "cg_out": cg_out,
        "exe_ok": exe_ok,
        "exe_out": exe_out,
    }


def main():
    if not MIMI.exists():
        print(f"mimi binary not found: {MIMI}", file=sys.stderr)
        print("Run: LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build --release", file=sys.stderr)
        sys.exit(1)

    files = sorted(p for p in ROOT.glob("*.mimi") if p.is_file())
    results = []
    for f in files:
        print(f"Running {f.name} ...", flush=True)
        results.append(run_one(f))

    # Package import test (local dependency)
    consumer = ROOT / "projects" / "consumer" / "main.mimi"
    if consumer.exists():
        print(f"Running package_import (consumer) ...", flush=True)
        results.append(run_one(consumer))

    # Summary
    print("\n" + "=" * 80)
    print(f"{'TEST':<40} {'RUN':>6} {'BUILD':>6} {'EXEC':>6}")
    print("-" * 80)
    total = len(results)
    run_pass = 0
    build_pass = 0
    exec_pass = 0
    for r in results:
        run_mark = "PASS" if r["interp_ok"] else "FAIL"
        build_mark = "SKIP" if not r["requires_codegen"] else ("PASS" if r["cg_ok"] else "FAIL")
        exec_mark = "SKIP" if not r["requires_codegen"] else ("PASS" if r["exe_ok"] else "FAIL")
        if r["interp_ok"]:
            run_pass += 1
        if r["cg_ok"]:
            build_pass += 1
        if r["exe_ok"]:
            exec_pass += 1
        print(f"{r['name']:<40} {run_mark:>6} {build_mark:>6} {exec_mark:>6}")

    print("=" * 80)
    print(f"Total: {total}  run: {run_pass}/{total}  build: {build_pass}/{total}  exec: {exec_pass}/{total}")

    # Detail failures
    failed = [
        r
        for r in results
        if not r["interp_ok"]
        or (r["requires_codegen"] and (r["cg_ok"] is not True or r["exe_ok"] is not True))
    ]
    if failed:
        print("\n--- Failure details ---")
        for r in failed:
            print(f"\n>> {r['name']}")
            if not r["interp_ok"]:
                print("[interp FAIL]\n" + r["interp_out"][:800])
            if r["requires_codegen"] and r["cg_ok"] is not True:
                print("[build FAIL]\n" + r["cg_out"][:800])
            if r["requires_codegen"] and r["exe_ok"] is not True:
                print("[exec FAIL]\n" + r["exe_out"][:800])

    sys.exit(0 if not failed else 1)


if __name__ == "__main__":
    main()
