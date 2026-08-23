# Phase F — 诊断可修复性轨迹（0.39.112 演示）

> 证明 `avg_fix_rounds` 可测量，且诊断本身给出可应用补丁（"诊断给可应用补丁"
> 是内核卡外的 AI 可写性支柱，见 `devdocs/kernel-and-ai-writability-2026-08-18.md`）。
> 本文件记录 hand-authored bad→good 轨迹；真实模型评测只改 `candidates/` 目录
> 与报告，不改本文件。

## 轨迹 1 — t02 线性泄漏（E0256 → 修复）

round 1 候选：`make_token()` 后未消费 `t`。

```text
error[E0256] ... linear resource 't' must be consumed before this return path
help: move, return, transfer, or drop the resource before returning
```

**修复**（round 2）：用唯一合法消费 `token_id(t)`（或 `token_channel_send` /
`*_guarded`），并 drop 返回值。

```text
let t = make_token()
let id = token_id(t)     // 恰一次消费
drop(id)
```

- round1：check=0（E0256）；round2：check=1 semantic=1 dual=1。
- 修复由**编译器直接给出**（E0256 + help 文本）；模型无需猜 API。

## 轨迹 2 — t05 错误输出（语义不匹配）

round 1 候选：`safe_div(10,2)` 打印 `v-1` → 输出 `4`，期望 `5`。check 过、
semantic 不过（harness 比对输出）。

**修复**（round 2）：改回打印 `v`。round2 全绿。

## 指标口径（run_eval.sh）
- `first_check`：取各任务 **round-1** 行的 first_check_ok。
- `semantic` / `escape`：取各任务 **最后一轮** 行。
- `avg_fix_rounds`：语义通过任务的最终轮号；未通过计 max_fix_rounds。
- 任务分组：`tXX.N.mimi` 的 round 后缀被剥离，同一任务多轮折叠为一个任务。

## 复跑
```bash
eval/run_eval.sh <candidates_dir> <out.csv>
# 基线（参考解）：all 1.00
# 对抗集：第一轮 check/semantic 正确归零、mms{} escape=1
# 修复轨迹集：first_check=0.50 semantic=1.00 avg_fix_rounds=2.00
```
