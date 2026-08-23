#!/usr/bin/env bash
# Phase F — 逐轮自动重试 driver（0.39.114）。
#
# 给定一个初始候选 + 一个修复钩子（repair hook），循环至多 MAX_ROUNDS 轮：
#   round 1 用初始候选，跑 harness；
#   若 semantic 未过且未达上限，调修复钩子产 round N+1 候选，继续。
# 修复钩子可以是：真实 LLM CLI、脚本化修复、或人工（把修复文件放到
# work_dir/round_N.mimi 供下一轮读取——本驱动支持该"热插拔"模式）。
#
# 用法：retry_loop.sh <task.mimi> <cand0.mimi> <expected> <work_dir> [max_rounds]
set -u
TASK="${1:?task}"; CAND0="${2:?cand0}"; EXPECTED="${3:?expected}"; WORK="${4:?work_dir}"
MAX="${5:-5}"
HARNESS="$(dirname "$0")/eval_harness.sh"
mkdir -p "$WORK"
OUT="$WORK/trajectory.csv"; : > "$OUT"
MIMI="${MIMI:-./target/debug/mimi}"

task="$(basename "$TASK")"
cur="$CAND0"; round=1; done=0
while [[ $round -le "$MAX" && "$done" -eq 0 ]]; do
  "$HARNESS" --task "$TASK" --candidate "$cur" --expected "$EXPECTED" --round "$round" --out "$OUT"
  # last CSV col = first_check for round1 else check; column 3 = semantic
  semantic="$(tail -1 "$OUT" | cut -d, -f4)"
  if [[ "$semantic" == "1" ]]; then done=1; echo "round $round PASS"; break; fi
  if [[ $round -eq "$MAX" ]]; then echo "round $round FAIL (max rounds)"; break; fi
  next="$WORK/round_$((round+1)).mimi"
  if [[ -f "$next" ]]; then
    echo "round $round fail; taking pre-staged $next"
  else
    echo "round $round fail; no staged fix (hook) for round $((round+1)) — stopping"
    break
  fi
  cur="$next"; round=$((round+1))
done
echo "--- trajectory ($task) ---"; cat "$OUT"
