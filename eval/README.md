# Mimi AI 可写性评测（Phase F，0.39.1xx）

> 目标：用内核卡 + 正例集对 AI 模型做可写性基线，产出**可复跑数字**
> （成功标准：`AI 有可复跑数字，不是观感`）。
> 依据：`devdocs/kernel-and-ai-writability-2026-08-18.md`（Hyp 裁决）、
> `devdocs/kernel-card.md`。

## 目录
- `tasks/` — 6 道任务（Flow/线性/Session/actor runs Flow/失败分层/CRUD）
  每道含规范正例 + 期望输出（`task_list.md`）。
- `eval_harness.sh` — 单候选门禁：check / semantic / dual / escape_abuse。
- `run_eval.sh` — 批量：折叠任务、聚合四指标。
- `retry_loop.sh` — 逐轮自动重试（读预置修复或接修复钩子）。
- `repair_hook.example.sh` — 修复钩子接口（默认 no-op；真实模型替换它）。
- `cluster_failures.py` — 失败模式聚类（check/parse、check/E0256、escape、
  semantic）。
- `freeze.toml` — 冻结清单（编译器版本/内核卡 SHA/模型/采样/最大修复轮）。
- 报告模板：`devdocs/v0.39/phase-f-report-template.md`；正式报告落
  `devdocs/v0.39/phase-f-report.md`。

## SOP（评测操作规程）
1. **冻结**：确认 `freeze.toml`（编译器 commit、内核卡 SHA、模型、采样、
   max_fix_rounds）。改动任何一项 = 评测失效，需重跑并注明。
2. **生成候选**：给模型每个任务只发「任务描述」（task_list.md 的「要求」列，
   不含正例），模型输出放 `candidates/<task_id>.<round>.mimi`。
3. **跑 harness**：
   ```bash
   eval/run_eval.sh candidates /tmp/results.csv
   eval/retry_loop.sh eval/tasks/t02_linear.mimi candidates/t02_linear.1.mimi \
       "t02_linear ok" work/ 5   # 自动重试
   ```
4. **聚类失败**：
   ```bash
   python3 eval/cluster_failures.py /tmp/results.csv candidates --group
   ```
5. **填报告**：按 `phase-f-report-template.md`（冻结清单 + CSV + 聚合 + 失败
   模式 + 结论）。评测失败 → 改诊断/正例，**不改种类规则**。

## 指标口径（run_eval.sh）
- `first_check`：各任务 **round-1** 行 first_check_ok。
- `semantic` / `escape`：各任务**最后一轮**行。
- `avg_fix_rounds`：语义通过任务的最终轮号；未通过计 max_fix_rounds。
- 任务折叠：`tXX.N.mimi` 剥离 round 后缀。

## 参考解守卫
`src/tests/phase_f.rs`（tracked）嵌入 6 道正例，断言 check + 双后端等价 +
无逃生舱——编译器回归使基线失效必红。

## 已知边界
- 本环境离线：`ollama` 可用但无本地模型 → 真实模型冒烟需在可拉取模型的机器
  上替换 `repair_hook` 为 LLM CLI。
- `token_id` 输出依赖进程级计数器：t02 正例用固定 marker（`t02_linear ok`），
  不依赖具体数值。
