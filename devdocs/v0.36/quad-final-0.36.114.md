# 0.1.6 四象限终测报告（0.36.114，Phase E/F）

> **里程碑**：0.1.6（内部 sprint 0.36.x）核心深度闭环终测。
> **范围**：Phase A–D 四支柱“定案 + 挣绿”复核 + Phase E 加固/审计台账闭合 +
> Phase F 终测/文档重锚。
> **对照**：延续 0.1.5 的 `devdocs/v0.35/quad-final-0.35.16.md` 证据风格；
> 本次终测以**全量门禁 + 文档一致性 + dispatch 基线**为主体。

---

## 1. 终测结论

0.1.6 核心四支柱（失败归属、状态语义、线性系统、语法重设计）以及 Phase D
语法定案均已达成“定案 + 挣绿”；Phase E 攻击面/审计台账在 0.36.56–114 全部
按设计闭合或显式延后；Phase F 全量复核与文档重锚完成。0.1.6 不再有未记录的
活缺口，Wave-3 / 0.2 / 1.x 边界已固化到 `known-boundaries-0.1.6.md`。

---

## 2. 测试门禁

| 门禁 | 命令 | 结果 | 耗时 |
|------|------|------|------|
| lib 全量 | `LLVM_SYS_181_PREFIX=… cargo test --lib -- --test-threads=4` | **5474 passed / 0 failed / 6 ignored** | 115.20s |
| real_world 集成 | `cargo test --test real_world -- --test-threads=4` | **31 passed / 0 failed** | 54.85s |
| real_world CLI 全语料 | `cargo test --test real_world_cli -- --test-threads=4` | **1 passed / 0 failed** | 117.29s |
| ASAN/工具 6 项 ignored | `cargo test --lib -- --ignored --test-threads=4` | **6 passed / 0 failed** | 0.15s |
| fmt | `cargo fmt --check` | ✅ 0 diff | — |
| clippy | `cargo clippy --all-targets -- -D warnings` | ✅ 0 warnings | 22.88s（增量复跑 0.06s） |
| 语言文档门禁 | `python3 scripts/check_language_docs.py` | ✅ 31 requirements / 31 support entries，semantic freshness 通过 | — |
| edge isolation 门禁 | `python3 scripts/check_edge_isolation.py` | ✅ 6 edge items registered，核心门禁零边缘依赖 | — |

---

## 3. dispatch 基线（完整 report）

`scripts/dispatch_stat.py report` 已完整跑完 128 个语料条目，无超时：

- 成功编译并产出 DispatchStats：**120**
- 跳过：**8**
  - FFI 示例/依赖外部库的 fixture（`demos/c_ffi_layer/strutil.mimi`、
    `demos/contract_ffi/contracts_simple.mimi`、`demos/python_ffi/math.mimi`、
    `demos/rust_ffi/rust_functions.mimi`、`examples/ffi/math.mimi`、
    `examples/ffi_verification.mimi`、`tests/real_world/projects/mylib/main.mimi`）
  - interpreter-only fixture（`tests/real_world/flow_test_macros.mimi`）
- 聚合：`total_functions=5832`，`eligible=4228`，`legacy_fallback=1604`，
  `fallback_rate=0.27503429355281206`
- 对 0.2 legacy 退役的起点基线：本报告 JSON 快照已随仓库保存为
  `devdocs/v0.36/dispatch-report-0.36.114.json`；如需更新正式基线，
  可执行 `scripts/dispatch_stat.py generate` 刷新
  `devdocs/v0.34/golden/dispatch-baseline.json`

> 说明：本报告为 `report` 模式（不做回退率对比）。跳过集均为非生产语料或
> interpreter-only 契约，不是 resolved/legacy 静默回退。
> 用同一 report JSON 对照旧基线 `devdocs/v0.34/golden/dispatch-baseline.json`：
> 既有 113 个程序 0 项回退率上升，7 个新程序自动纳入，0 个旧程序缺失。

---

## 4. 文档重锚（Phase A–D 定案写入 golden/spec/syntax-reference）

- **golden 语法**：`devdocs/v0.34/golden/syntax-reference.golden.md` 版本同步为
  `0.1.6-dev (internal sprint 0.36.X)`；Phase D 的 T? 删除、
  `defer/on failure` 单轨、关键字 63 词等已由 `docs/syntax-reference.md`
  同步承载。
- **spec**：`docs/language-spec.md` 已含 Phase A（StateId/EventId 名义化、
  recover/reset 语义）、Phase B（Actor mut / Protocol 静态投影定案）、
  Phase C（Session residual 双后端闭环）与 Phase D（guard/软关键字/`?` 去歧义）。
  本次终测更新 §3.10 “not yet closed” 列表，移除已闭环的 codegen residual
  lowering，保留 cross-turn exactly-once / Fault-path cleanup / advanced Session。
- **language-support.toml**：`implementation_version` 同步为 0.1.6-dev；
  `SESSION-LINEAR-001` 证据更新为已闭环的 triple-harness Session residual 矩阵。
- **README 系列**：README.md / README.zh.md 的版本徽标、状态段落、版本历史表
  同步为 0.1.6-dev，并指向本报告与 known-boundaries。
- **production 路径证据**：新增 `dual_production_checked_path_smoke`，对
  `compile_checked` 生产路径与 VM、E2E native 三方对拍；fully tracked 于
  `src/tests/dual_backend.rs`。
- **AGENTS 版本表**：0.1.6 行更新为“Phase A–D 已定案 + 挣绿；Phase E/F 终测
  与文档重锚完成”。

---

## 5. 已知边界 / 已知延后

已固化到 `devdocs/v0.36/known-boundaries-0.1.6.md`，摘要：

- **Wave-3 结构项**：三引擎逐特性矩阵、bindgen 完全 Component IR、Wire schema
  传输接线、列表所有权模型、Go 回调 ABI 根解、spawn/await 真实并发、JSON serde
  结构统一。
- **0.2**：rich fault variant 集、Component IR/Wire 扩张、FFI slice/buffer、
  native/VM 性能目标、ABI 版本握手实施。
- **1.x 评估**：match guard CFG 精确分叉、宽度模型统一、线性容器非定向提取、
  提前退出逐元素记账、双引擎整数模型统一。
- **边缘 6 项**：见 `edge-inventory.toml`，均保持核心门禁零依赖。

---

## 6. Phase A–D 定案快照

| Phase | 定案 | 挣绿证据 |
|-------|------|----------|
| A 失败归属 | Fault payload 名义化（StateId/EventId）；recover 穷尽 match；Fault≠Result；二次 Fault/Reset 语义 | fault_nominal gate 零字符串状态编码；`flow_features` / dual_backend 回归 |
| B 状态语义 | Actor mut = 简单状态逃生舱标记；Protocol = checker-only 静态投影；dyn Protocol = 稳定逃生舱 | spec §3.8/§3.9/§6.4/§6.5 定稿；protocol conformance 正/负例 + dyn 双后端 |
| C 线性系统 | 泛型×线性黑盒 + 穷举解构/循环/if-let/定向头提取/容器方法面贯通；Session typed 端点 + residual 双后端 | 0.36.36–49 切片双后端回归；`dual_linear_*` / `dual_session_*`；ASAN 6 项本次复跑 |
| D 语法重设计 | 关键字 63 词；`T?` 删除；`defer`/`on failure` 双表面单轨；软关键字政策 | `check_phase_d_syntax_gate` 全绿；`keyword_table_count_*` / `all_soft_keywords_bindable_in_let` 等回归 |

---

*报告生成：2026-08-15，0.36.114。*
