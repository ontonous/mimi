#!/usr/bin/env bash
# Phase F — AI 可写性评测 harness（0.39.111 起）。
#
# 对单个候选程序跑一套固定门禁，产出可复跑指标行：
#   task, round, check, semantic, dual, escape_abuse, first_check_ok
#
# 用法：
#   eval_harness.sh --task <task.mimi> --candidate <cand.mimi> \
#       [--expected "<stdout>"] [--round N] [--out <file.csv>]
#
# 门禁定义（与内核卡 §5-6 一致）：
#   check         : `mimi check` 通过
#   semantic      : `mimi run` 输出与期望完全一致
#   dual          : `mimi run` 与 `mimi build`（native）输出一致
#   escape_abuse  : 候选是否使用出核构造（cap 声明、mms{}、thread_local cap）
#   first_check_ok: round==1 时 check 是否直接通过（首次 check 率）

set -u

TASK="" CAND="" EXPECTED="" ROUND=1 OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --task) TASK="$2"; shift 2;;
    --candidate) CAND="$2"; shift 2;;
    --expected) EXPECTED="$2"; shift 2;;
    --round) ROUND="$2"; shift 2;;
    --out) OUT="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
[[ -n "$TASK" && -n "$CAND" ]] || { echo "need --task and --candidate" >&2; exit 2; }
[[ -f "$CAND" ]] || { echo "candidate not found: $CAND" >&2; exit 2; }

MIMI="${MIMI:-./target/debug/mimi}"
[[ -x "$MIMI" ]] || { echo "mimi binary missing at $MIMI (run cargo build first)" >&2; exit 2; }

task="$(basename "$TASK")"
check="0"; semantic="0"; dual="0"; escape="0"

# 0) escape-hatch abuse FIRST: source-level, independent of check/semantic.
#    mms{} was removed in 0.1.8; thread_local cap is deprecated (Phase D).
#    `cap` declarations stay IN kernel as the canonical linear value, so they
#    are NOT flagged here.
if grep -Eq 'mms\{|thread_local|thread-local' "$CAND"; then
  escape="1"
fi

# 1) check
if "$MIMI" check "$CAND" >/dev/null 2>&1; then
  check="1"
else
  first="0"; [[ "$ROUND" == 1 ]] && first="0"
  echo "$task,$ROUND,$check,$semantic,$dual,$escape,$first" >> "${OUT:-/dev/null}"
  exit 0
fi

# 2) semantic (VM output == expected)
vm_out="$("$MIMI" run "$CAND" 2>&1)"
if [[ -n "$EXPECTED" ]]; then
  if [[ "$vm_out" == "$EXPECTED" ]]; then semantic="1"; fi
fi

# 3) dual (VM == native)
native_out=""
if "$MIMI" build "$CAND" -o "/tmp/eval_$$" >/dev/null 2>&1; then
  native_out="$("/tmp/eval_$$" 2>&1)"
  rm -f "/tmp/eval_$$"
  [[ "$vm_out" == "$native_out" ]] && dual="1"
fi

# 4) first_check_ok: round-1 check directly passed.
first="0"; [[ "$ROUND" == 1 ]] && first="$check"
echo "$task,$ROUND,$check,$semantic,$dual,$escape,$first" >> "${OUT:-/dev/null}"
exit 0
