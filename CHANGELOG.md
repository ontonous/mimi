# Changelog

## [Unreleased] — 0.1.1-dev

### io.rs 格式化 dispatch 重构（Sprint A1–A2 完成）

- 46 个 `emit_map_*` 近重复函数合并为参数化 `emit_map_container_product_to_json` + `product_tuple_arity` 辅助函数（-1544 行）。
- `extract_print_arg` 的 Map/Set 分发树（1886 行手工展开 if-else）替换为 `resolve_container_product` 递归类型解析器（-1693 行）。
- io.rs: 14658 → 11421 行（-22%），双后端 735/0 等价确认。

### FLOW-IDENTITY-001 状态身份（0.31.8 完成）

- **E0421 状态不可伪造**：非根 flow state 在 transition 体外构造被拒绝。根状态（flow 第一个 state）构造不受影响（Flow 构造器语义）。3 个新测试。
- **E0422 名义状态区分**（warning）：跨流同名义状态 payload 兼容时发出 warning，提示使用限定名 `flow::<flow_name>::<state_name>`。
- **E0423 线性 generation**：flow state 变量被 transition 消费后，后续使用被静态拒绝。Checker 层 `consumed_flow_vars` 追踪 + `lookup_var` 拦截；解释器 `mark_moved` safety net。2 个新测试。`flow_counter.mimi` 和 `flow_codegen_chain` 中的 use-after-transition 已修正。

### FLOW-TURN-001 原子 Turn（0.31.9 完成）

- **`fails E` 语法**：transition 签名支持 `-> Target fails ErrorType`，声明可回滚失败路径。Lexer 新增 `fails` 关键字，Parser 解析 `fails E`，AST `TransitionDef.fails: Option<Type>`。
- **E0424**：transition 体内 `?` 无 `fails E` 声明时静态拒绝。
- **Rejected 路径（解释器）**：`?` 失败时 transition 返回 `Err((source_payload, error))`，source generation 归还调用方。修复 `early_return` 泄漏 bug（transition 体内 `?` 的 `early_return` 不再穿透到调用方）。4 个新测试。
- **Codegen fail-closed**：`fails E` transition 中 `?` 在 codegen 报 `CompileError::Unsupported`（E0722），防止静默产生错误行为。无 `?` 的 `fails E` transition 正常编译。
- **`transition_fails_types` 基础设施**：Checker 存储每个 transition 的 `fails E` 类型。
- **返回类型 `Result<Target, (Source, E)>`**：Checker 注册 `fails E` transition 返回类型为 `Result<Target, (Source, E)>`；Resolved IR `ResolvedTransition.fails` 字段 + canonical 签名同步包装；IR Lower `transition_fails` 标志使 return 语句期望内层 Target 类型；Interpreter 成功路径包装 `Ok(v)`。测试更新为 match Ok/Err 模式。
- **`become`/`stay` 显式 terminal 关键字**：`become Expr` 构造目标状态并结束 transition（等价于 return）；`stay` 返回 source 状态不变（自环终端）。全栈实现：Lexer/Parser/AST/Checker/IR Lower/CFG/Interpreter/Codegen（block.rs + func.rs）。5 个新测试（含双后端等价）。
- **Codegen Rejected 完整镜像**：`fails E` transition 中 `?` 在 codegen 不再 fail-closed（E0722 已移除）。Rejected 路径构造 `Err((source, error))` 并返回；成功路径包装 `Ok(target)`。`transition_to_func` 返回类型变为 `Result<Target, (Source, E)>`。2 个新双后端测试。
- **Draft isolation**：transition 体内 `self` 为不可变参数（`mut_: false`），source 在 Rejected 时原样归还。原子 turn 保证：transition 要么成功返回 Ok(target)，要么失败返回 Err((source, error))，不存在中间状态泄漏。
- **已知限制**：codegen match on Result with record payloads 不支持对绑定变量的字段访问（`var_type_names` 未注册 Ok payload 类型名），需用 `Ok(_)`/`Err(_)` 模式。

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
