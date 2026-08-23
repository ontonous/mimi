#!/usr/bin/env bash
# Phase F — 批量评测 + 聚合指标（0.39.111 起）。
#
# 用法：run_eval.sh <candidates_dir>
#   candidates_dir 下放候选解答，命名 <task_id>.<round>.mimi（如 t01_flow.1.mimi）。
#   也支持直接把正例目录传进来做 harness 自检（baseline = 参考解全过）。
#
# 输出：CSV（逐行指标）+ 聚合（首次 check 率/语义测试率/平均修复轮/逃生滥用率）。
set -u
CAND_DIR="${1:?need candidates_dir}"
OUT="${2:-/tmp/phase_f_results.csv}"
HARNESS="$(dirname "$0")/eval_harness.sh"
MIMI="${MIMI:-./target/debug/mimi}"
: > "$OUT"

declare -A EXPECTED=(
  [t01_flow]="t01_flow ok"
  [t02_linear]="t02_linear ok"
  [t03_session]="t03_session ok"
  [t04_actor_flow]="t04_actor_flow ok"
  [t05_failure]="5"
  [t06_crud]="2
a
true"
)

MAX_ROUNDS="${MAX_ROUNDS:-5}"
for f in "$CAND_DIR"/*.mimi; do
  base="$(basename "$f")"
  task="${base%%.*}"
  [[ -n "${EXPECTED[$task]+x}" ]] || continue
  round="$(echo "$base" | sed -n 's/^[^.]*\.\([0-9]*\)\.mimi$/\1/p')"
  [[ -n "$round" ]] || round=1
  exp="${EXPECTED[$task]}"
  "$HARNESS" --task "$f" --candidate "$f" --expected "$exp" --round "$round" --out "$OUT"
done

# 聚合（从候选目录中取各 task 最后出现 round 的 check/semantic 作为最终判定）
python3 - "$OUT" "$CAND_DIR" "$MAX_ROUNDS" <<'PY'
import csv, glob, os, sys
out, cand_dir, max_rounds = sys.argv[1], sys.argv[2], int(sys.argv[3])
rows = list(csv.reader(open(out)))
if not rows:
    print("no rows"); sys.exit(0)
# Group rows by task id: strip the `.<round>.mimi` suffix so round files of the
# same task collapse into one task (fix-round trajectory aggregation).
def task_of(r):
    return r[0].rsplit(".", 2)[0]
tasks = sorted(set(task_of(r) for r in rows))
n = len(tasks)
def last(task):
    best = None
    for r in rows:
        if task_of(r) == task:
            rd = int(r[1]); best = r if best is None or rd > int(best[1]) else best
    return best
def first_row(task):
    best = None
    for r in rows:
        if task_of(r) == task:
            rd = int(r[1]); best = r if best is None or rd < int(best[1]) else best
    return best
# first_check uses the ROUND-1 row (first_check_ok); semantic/escape use the
# LAST (final) row of the task.
first_check = sum(1 for t in tasks if (r := first_row(t)) and r[6] == "1")
semantic = sum(1 for t in tasks if (r := last(t)) and r[3] == "1")
escape = sum(1 for t in tasks if (r := last(t)) and r[5] == "1")
fix_rounds = []
for t in tasks:
    r = last(t)
    if r and r[3] == "1":
        fix_rounds.append(int(r[1]))
    else:
        fix_rounds.append(max_rounds)
print(f"tasks={n} first_check_rate={first_check/n:.2f} semantic_test_rate={semantic/n:.2f} avg_fix_rounds={sum(fix_rounds)/n:.2f} escape_abuse_rate={escape/n:.2f}")
PY
echo "--- rows ---"; cat "$OUT"
