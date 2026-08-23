#!/usr/bin/env python3
"""Phase F — 失败模式聚类。

读取评测 CSV + 候选目录，对每道失败任务（semantic != 1 或 escape == 1）按
round-1 候选跑 `mimi check` 提取错误码，聚合成表：
  task | mode | code | note
mode ∈ {check, semantic, escape}
"""
import argparse, csv, os, re, subprocess, sys
from collections import Counter

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("csv", help="results CSV")
    ap.add_argument("cand_dir", help="candidate .mimi dir")
    ap.add_argument("--mimi", default="./target/debug/mimi")
    ap.add_argument("--group", action="store_true")
    a = ap.parse_args()

    rows = list(csv.reader(open(a.csv)))
    if not rows:
        print("no rows"); return 0
    tasks = {}
    for r in rows:
        task = r[0].rsplit(".", 2)[0]
        tasks.setdefault(task, []).append((int(r[1]), r))
    failures = []
    for task, rs in tasks.items():
        last = max(rs, key=lambda x: x[0])[1]
        if last[3] == "1" and last[5] == "0":
            continue
        cand = None
        for name in os.listdir(a.cand_dir):
            if name.rsplit(".", 2)[0] == task and name.endswith(".mimi"):
                p = os.path.join(a.cand_dir, name)
                if cand is None or name < os.path.basename(cand):
                    cand = p
        mode = "semantic"; code = "-"; note = ""
        # escape is source-level (takes precedence over check/semantic)
        if last[5] == "1":
            mode = "escape"; code = "-"; note = "out-of-kernel construct"
        elif cand and last[2] == "0":
            cp = subprocess.run([a.mimi, "check", cand],
                                capture_output=True, text=True)
            out = cp.stdout + cp.stderr
            m = re.search(r"\[(E\d{4})\]", out) or re.search(r"error\[(E\d{4})\]", out)
            code = m.group(1) if m else "parse/other"
            mode = "check"
        failures.append((task, mode, code, note))
    failures.sort()
    for t, mode, code, note in failures:
        print(f"{t:14s} {mode:10s} {code:12s} {note}")
    if a.group:
        print("\n--- summary ---")
        for (mode, code), n in Counter((m, c) for _, m, c, _ in failures).items():
            print(f"{mode:10s} {code:12s} x{n}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
