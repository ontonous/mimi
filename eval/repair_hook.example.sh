#!/usr/bin/env bash
# Phase F — 修复钩子接口（示例/默认实现）。
#
# retry_loop.sh 在 round N 失败后调用此钩子生成 round N+1 候选：
#   repair_hook.sh --task <task.mimi> --candidate <cand.mimi> \
#       --diagnostic <diag.txt> --round <N> --next <out.mimi>
#
# 接口契约：
#   - 读取 --candidate 与 --diagnostic（mimi check/run 的诊断输出）；
#   - 产出修复后的程序写入 --next；
#   - 退出码 0 = 产出可用；非 0 = 本轮无修复（retry_loop 停止）。
#
# 真实模型接入：把本文件替换为调用 LLM CLI 的脚本（提示词 = task 要求 +
# 内核卡 + 候选 + 诊断 → 生成 --next）。默认实现仅拷贝（no-op）。
set -u
TASK=""; CAND=""; DIAG=""; ROUND=1; NEXT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --task) TASK="$2"; shift 2;; --candidate) CAND="$2"; shift 2;;
    --diagnostic) DIAG="$2"; shift 2;; --round) ROUND="$2"; shift 2;;
    --next) NEXT="$2"; shift 2;; *) echo "unknown: $1" >&2; exit 2;;
  esac
done
[[ -n "$CAND" && -n "$NEXT" ]] || { echo "need --candidate and --next" >&2; exit 2; }
cp "$CAND" "$NEXT"
exit 0
