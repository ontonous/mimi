# Changelog

## [Unreleased] — 0.1.1-dev

## [0.1.0] — 基线稳定 - 2026-07-23

### 止血 II 收尾 + 版本管理切换 + 架构重构

- **版本管理切换**：外部版本从 `mimi-v0.31.X` 切换为纯 semver（`0.1.0`、`0.1.1`、...、`1.0.0`）。旧 `mimi-v*` tag 保留为开发历史，不再新增。内部 sprint 仅体现在 commit message 中。
- **架构重构（0.1.0 收尾）**：
  - `src/runtime/mod.rs` 拆分（24105→18142 行，14 个模块）：regex/lexer/crypto/fs/binary_io/future/ffi_test/concurrency/actor/quote/net/shadow_mte/capability/env 抽出，机械拆分不改语义，419 个 `#[no_mangle]` 符号全导出、4053 测试绿。硬共享簇（map/set/list/string/json ~180 extern fn）函数交错且互引，作为耦合核心保留 mod.rs。
  - `src/core/resolved.rs` 拆分（12702→8551 行）：目录化为 resolved/mod.rs，`#[cfg(test)] mod tests`（4129 行）分离到 resolved/tests.rs。identity/catalog/walk 生产代码边界模糊且重度耦合，作为耦合核心保留 mod.rs。
- 止血 II 修复项（按信任链排序，逐项完成后登记）：
  - **F1 测试 oracle**：删除进程级 `GLOBAL_STDOUT_CAPTURE` 全局槽与 `resolve_stdout_buf` fallback，消除并行测试 stdout 串扰。
  - **silent error 止血**：codegen 12 处 `let _ = build_store/build_call` 改传播；`test_sandbox` spawn 失败如实报告。
  - **文档真值**：`AGENTS.md` §13/§0 重新对齐（函数体层仍经 `legacy_body_file()`、线性能力 0.1.1 前零强制）。
  - **CI 门禁**：`LLVM_SYS_181_PREFIX` 修正、clippy `--all-targets`、分级门禁、unsafe SAFETY baseline 锁定。
  - **测试质量**：清零走过场测试、`v1_4` 家族强制 L1、real_world golden（增量）。

> 开发历史：1863 commits，66 个 `mimi-v*` tag（v0.12.0–v0.31.6），38 天（2026-06-15 至 2026-07-22）。
> 详细施工记录见 `devdocs/archive/` 和 git log。

### 里程碑

- **CheckedProgram 语义中枢**：唯一语义真值源，持有 canonical 签名、Flow transition 表、Actor/Session/Protocol 目录、ownership action summaries、CFG。
- **Typed Resolved IR**：ResolvedFunction/ResolvedFlow/ResolvedTransition/ResolvedActor 等 canonical 声明（12.7k LOC）。
- **HM Unification**：undo trail + TypeScheme + zonk；泛型调用 fresh instantiate。
- **CFG/Ownership 分析**：per-callable 控制流图 + stable-ID CallableCfg + 线性资源 ledger（Introduce/Move/Drop/Return + borrow）。
- **止血 I/II**：测试 oracle 修复、silent error 传播、文档真值对齐、CI 门禁强化、Clippy 基线清零。
- **双后端等价**：4063 测试（4053 passed / 0 failed / 10 ignored），69 个 real_world 程序双后端 68/69 通过（`flow_test_macros.mimi` 为 interpreter-only，不参与双后端比对）。
- **Flow 范式**：38 项白皮书能力全部达成（v0.29 冻结），双后端 stdout 等价。
- **stdlib**：io/fs/strings/collections/json/csv/crypto/maps/mymath/net/time/datetime/env/testing/regex/template/set。
- **工具链**：mimi check/run/build/verify/fmt/lint/lsp/init/add/install/tree。

### 已知限制（0.1.0 基线）

- 线性能力仅有分析，零用户可见强制（exactly-once 闭环排入 0.1.1）。
- Flow 转移无原子 terminal model（atomic turn 排入 0.1.1）。
- Session 端点运行时可退化为整数（typed residual 排入 0.1.1）。
- Component IR / ABI / Wire 不存在（排入 0.1.3）。
- 函数体仍经 `legacy_body_file()` 消费 raw AST（迁移排入 0.1.1）。

---

## Pre-0.1.0 时代摘要

> 详细施工日志（v0.1.0–v0.31.6，1863 commits，66 个 `mimi-v*` tag）保留在 git 历史中
> （`git log -- CHANGELOG.md`），本地归档副本见 `devdocs/archive/CHANGELOG-pre-0.1.0.md`。

| 时代 | 版本范围 | 日期 | 主题 |
|------|---------|------|------|
| 原型 | v0.1.0–v0.7.0 | 06-15 ~ 06-17 | 解释器 + 类型检查器 + CLI 原型 |
| 筑基 | v0.12.0–v0.20.1 | 06-23 | 控制流、函数、类型系统、stdlib 基础 |
| 补全 | v0.21.0–v0.27.6 | 06-24 ~ 06-26 | JSON、LSP、pipe/loop、Z3 验证器、结构化并发、安全审计 |
| 使用驱动 | v0.28.0–v0.28.37 | 06-27 ~ 07-03 | 7 语言 FFI、profiler、bindgen、包管理器；Feature Bugs 清零 |
| Flow 范式 | v0.29.0–v0.29.41 | 07-03 ~ 07-12 | 编译器内部 Flow 替换（Parser→Lexer→Loader→LSP→Interp→Verifier→Checker）+ 语言级 Flow 语义 + 白皮书 38 项能力全部达成 |
| 止血 | v0.30.0 | 07-14 | 0 新 Feature — 15 项架构债务清零（sprintf→snprintf、路径安全、malloc 检查等） |
| 语义中枢 | v0.31.0–v0.31.6 | 07-15 ~ 07-22 | CheckedProgram / HM unification / CFG / Resolved IR / 止血 I/II → 汇入 0.1.0 |
