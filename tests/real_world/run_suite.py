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


def run_one(path: Path):
    rel = path.relative_to(ROOT)
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

    # Codegen
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

    # If codegen produced an executable, run it
    # mimi build defaults to ./<stem> (CWD), not next to the source
    exe = REPO_ROOT / path.stem
    exe_ok = None
    exe_out = ""
    if cg_ok and exe.exists():
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
        # clean up generated executable
        try:
            exe.unlink()
        except FileNotFoundError:
            pass

    return {
        "name": str(rel),
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
        build_mark = "PASS" if r["cg_ok"] else "FAIL"
        exec_mark = "PASS" if r["exe_ok"] else ("N/A" if r["exe_ok"] is None else "FAIL")
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
    failed = [r for r in results if not r["interp_ok"] or not r["cg_ok"] or r["exe_ok"] is False]
    if failed:
        print("\n--- Failure details ---")
        for r in failed:
            print(f"\n>> {r['name']}")
            if not r["interp_ok"]:
                print("[interp FAIL]\n" + r["interp_out"][:800])
            if not r["cg_ok"]:
                print("[build FAIL]\n" + r["cg_out"][:800])
            if r["exe_ok"] is False:
                print("[exec FAIL]\n" + r["exe_out"][:800])

    # Return non-zero if any interpreter run failed (baseline must pass)
    sys.exit(0 if run_pass == total else 1)


if __name__ == "__main__":
    main()
