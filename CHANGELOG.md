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

### 0.31.10 稀疏图 + typed Fault + 显式 reset/recover（进行中）

- **Per-Flow typed Fault**：`fault ErrorType` 声明语法，注入的 Fault 状态携带 `error: ErrorType` 字段。回退 transition 自动填充默认值。2 个新测试（含双后端）。
- **@sparse 稀疏图**：`@sparse` bare annotation 跳过 N×M fallback 注入。未声明的 (state, event) 对产生编译时错误而非自动路由到 Fault。2 个新测试。
- **显式 reset/recover**：用户自定义 `transition reset(Fault) -> State` / `transition recover(Fault) -> State` 覆盖自动注入的系统动词。2 个新双后端测试验证覆盖行为。
- 待实现：progressive Main 真 lowering（main 函数体作为 transition body 参与 Resolved IR lowering）。

### 0.31.11 Actor runs Flow（进行中）

- **`actor Name runs FlowName` 语法**：AST `ActorDef.runs_flow: Option<String>`，Parser 解析 `runs` soft keyword，Checker 验证引用的 flow 存在（E0402）。
- **Interpreter 集成**：`ActorInstance` 新增 `runs_flow` + `flow_state` 字段。spawn 时初始化 flow_state 为 root state（默认值）。Worker thread dispatch：`runs_flow` 设置时消息路由到 Flow transition table（从 flow_state 提取当前状态名 → 查找 (from_state, event) 匹配的 transition → `eval_flow_transition` 执行原子 turn → 更新 flow_state）。
- **mut 字段禁止**：`runs_flow` actor 的 `mut` 业务字段被 E0402 拒绝（状态由 Flow 携带）。
- **测试**：`actor_runs_flow_dispatch_through_transition` 验证 Zero→Positive→Positive 多 turn 累积（s3.n == 2）。
- 待实现：Codegen actor runs flow（需要 tagged-union state 存储 + state-dependent dispatch，与当前 flat-struct actor 模型不兼容，需专门设计）。

### 审查修复（0.31.9–0.31.11 事后审查）

- **C1 `block_returns_on_all_paths` 不认识 `Become`/`Stay`**：match 缺少分支导致 E0255 误报，CLI 拒绝合法 `become`/`stay` 代码。测试因 `run_source_result` 跳过 checker 而漏网。修复：添加 `Stmt::Become(_) | Stmt::Stay => return true`。
- **C2 Rejected 路径 error 双重包装**：`eval_try` 设 `early_return = Some(Err(e))`（完整 variant），Rejected 路径再包一层 `Err((source, Err(e)))`。修复：解包 variant 取内层 error 值。
- **H1 CFG 中 `Stay` 是 no-op**：`stay` 后的代码在 CFG 中仍可达，影响 ownership 分析。修复：标为 `Terminator::Return`。
- **H2 `Stay` 无类型验证**：注释声称 checker 验证 source 类型匹配，但无代码。修复：`self` 类型与返回类型 unify 失败时 E0209。
- **附加：`become`/`stay` 不再设 `early_return`**：仅 `?` 使用 Rejected 信号，避免 `become` 在 `fails E` transition 中误触发 Rejected 路径。

### 0.31.12 Typed Session Residual（完成）

- **E0425 scope exit 检查**：函数结束时，非 `end` residual 的 session endpoint 被拒绝。endpoint 必须完成协议（send/recv/close）或显式 return/transfer。
- **E0426 use-after-alias**：`let b = a`（a 是 session endpoint）后，a 被标记为 consumed，再用 a 触发 E0426（线性消费）。
- **Alias residual 转移**：`let b = a` 将 residual 从 a 转移到 b，a 的 residual 被移除。
- **Branch merge 一致性**：if/else 两分支的 session residual 必须一致才能 merge，分歧时 E0425。无 else 分支时保守恢复 pre-branch 状态。
- **测试基础设施（H3）**：新增 `checked_run_source_result` / `checked_compile_and_run`（checker + 后端），迁移 0.31.9–0.31.11 测试到 checked helper。
- 6 个新测试：alias 转移、use-after-alias、scope exit 拒绝/通过、branch merge 一致/分歧。

### 0.31.13 Resource exactly-once（进行中）

- **Session endpoint 函数参数 move**：session endpoint 作为函数参数传递时消费 residual，修复 E0425 误报，正确报 E0304 (moved after consumed)。
- **Session 线性回归验证**：double-close (E0304)、branch partial consume (E0425)、move-to-function (E0304) 三个场景确认 CFG dataflow 覆盖。
- **Cap 闭包 capture**：已有 TransferChild 分析 + E0304 强制（`ownership_checker_rejects_implicit_nested_capability_capture`），无需新增。
- 3 个新测试：session_double_close_rejected、session_branch_partial_consume_rejected、session_endpoint_move_to_function_rejected。
- **追加 A — Flow 状态别名追踪 + shared/ref 拒绝**：
  - `let b = s0`（s0 是 flow state）消费 s0，后续使用 s0 触发 E0423（对标 session E0426 机制）。
  - `shared`/`local_shared`/`weak`/`weak_local` 包装 flow state → E0427 拒绝（线性资源不允许多重引用）。
  - `let ref r = flow_state` → E0427 拒绝（借用隐含原值仍可用，违反线性）。
  - 删除 `consumed_flow_vars.remove(name)` shadowing 清除逻辑——shadowing 不重置线性消费（保守策略，0.31.16 CFG place 追踪修正）。
  - `flow_state_type_names: HashSet<String>` 注册所有 flow state 类型名（qualified + unqualified）。
  - CFG `is_linear()` 预留 transition `self` 跳过逻辑（0.31.16 启用 FlowStateSet + state: Nominal）。
  - 5 个新负测试。4098 测试全绿。
- 待实现：cross-turn exactly-once（Flow transition 间资源跟踪）、Fault path 资源清理。

### 0.31.14 Static Protocol Stable（进行中）

- **移除 deprecated `protocol_methods`**：spec 标记 `[removed]`，从 builtins/inference/codegen/interpreter 全部清除。Protocol 是纯编译期拓扑检查，不需要运行时反射。
- **Protocol 测试迁移**：4 个双后端测试迁移到 checked helper。
- **追加 A — Protocol conformance × 线性检查**：
  - Protocol state payload 线性匹配：protocol 声明线性 payload (Cap, SessionChan) 时，flow state 对应字段必须也是线性类型，降级 → E0427。
  - 3 个新测试：alias bypass (E0423)、alias target valid、payload downgrade (E0427)。
- 待实现：permission/effect 约束检查、fault 暴露策略、版本握手（需 Component IR，Phase C）。

### 0.31.15 Canonical Semantic Trace（基础设施完成）

- **TraceEvent / TraceCollector / compare_traces**：canonical 语义追踪基础设施（`src/trace.rs`），记录 Transition + Fault 事件，5 个单元测试。
- **Interpreter 集成**：`trace_collector` 字段 + `eval_flow_transition` 中 transition/Fault 事件记录。
- **`run_source_with_trace` 测试 helper**：trace 收集测试基础设施。
- **追加 A — 所有权转移事件 + generation 失效记录**：
  - `OwnershipTransfer` 事件：记录 flow state 所有权转移时刻（from_var → to_var，generation 失效精确位置）。
  - `LinearViolation` 事件：记录运行时 use-after-move 安全网诊断路径。
  - `compare_traces()` 扩展：generation_before/generation_after 参与比较（happens-before DAG generation 边）。
  - 3 个新单元测试。4104 测试全绿。
- 待实现：session/actor trace 记录、双后端 trace 比较测试。

### 0.31.16 Flow 状态 CFG 级线性（核心完成）

- **`is_linear()` 纳入 Flow 状态**：`FlowStateSet`（multi-target 结果）和 `state:` 前缀 Nominal（individual flow state）在 CFG dataflow 中是线性资源。
- **Auto-droppable**：Flow 状态代表数据，scope exit 时可安全丢弃（与 Cap/SessionChan 必须显式消费不同）。`ActionEmitter` 收集 flow state 局部变量作为 droppable 集合，`validate_return_resources` 跳过。
- **Transition `self` 隐式消费**：`build_resource_catalog` 和 `introduce_parameters` 跳过 transition 首参。
- **`_` 前缀 auto-drop**：`_d` 等 intentionally unused 变量不报 E0256。
- **`consumed_flow_vars` 保留为诊断增强层**：E0423 带 transition 名（比 CFG 的 E0304 更友好），CFG dataflow 是强制层。
- **Channel/Mutex/Atomic 遗留**：builtin 函数（整数 handle），非 ResolvedType Nominal，`is_linear()` 无法覆盖，留给后续类型表示升级。
- 4104 测试全绿。

### 0.31.19 攻击审查 I（完成）

- **审查范围**：0.31.16–18 闭环后的地基层（Flow 线性完备性、generation 失效、Actor×Flow 边界、Session×Flow 交互、双后端一致性、错误信息）。
- **P1 发现 + 修复**：tuple 构造 flow state 不消费原变量（`let t = (s0, 42); Counter::inc(s0)` 通过）→ `infer_tuple_expr` 加 `is_flow_state_type` 检查，E0427 拒绝。
- **线性完备性审查结果**（10 条攻击路径全部静态拒绝）：

| 攻击路径 | 诊断 | 层 |
|----------|------|-----|
| use-after-transition | E0423 | checker |
| alias chain (`let b = s0; let c = b; use(b)`) | E0423 | checker |
| self-loop double-use | E0423 | checker |
| function param move | E0304 | CFG |
| closure capture | E0427 | checker |
| list literal | E0427 | checker |
| map value | E0427 | checker |
| **tuple construction** | **E0427** | **checker (本次修复)** |
| shared/ref wrapping | E0427 | checker |
| shadowing no-reset | E0423 | checker |

- **错误信息质量**：E0423 带 transition 名 + help 文本；E0427 带类型名 + help 文本；E0304 (CFG) 无 transition 名（consumed_flow_vars 诊断增强层补充）。
- **Known limitation**：Channel/Mutex/Atomic 非 ResolvedType Nominal，is_linear() 无法覆盖；consumed_flow_vars 名字追踪保留为诊断层。
- **P0 = 0**。审查报告归档于 CHANGELOG。
- 4109 测试全绿。

### 0.31.18 证据同步与回归扫描（完成）

- **language-support.toml 全面更新**：implementation_version 更新至 0.1.1-dev (sprint 0.31.17)，8 个 requirement evidence 更新。
- **Clippy/fmt 修复**：`!is_ok()` → `is_err()`，`run_source_with_trace` dead_code allow。
- **回归扫描**：4108/0/10 全绿，clippy 0 warnings，fmt clean，real_world 70/70 run / 69/70 build。
- **Deferred 项清点（0.31.8–17）**：

| 项 | 来源 | 状态 | 去向 |
|---|---|---|---|
| progressive Main 真 lowering | 0.31.10 | 推迟 | 需 Resolved IR 级设计（broke 23 golden IR tests） |
| Codegen actor runs flow | 0.31.11 | 推迟 | 需 tagged-union state 存储设计 |
| cross-turn exactly-once | 0.31.13 | 推迟 | Flow transition 间资源跟踪，需 CFG 扩展 |
| Fault path 资源清理 | 0.31.13 | 推迟 | 需 Fault 路径 resource analysis |
| permission/effect 约束 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| fault 暴露策略 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| 版本握手 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| session/actor trace 记录 | 0.31.15 | 推迟 | 需 interpreter session/actor 路径集成 |
| 双后端 trace 比较测试 | 0.31.15 | 推迟 | 需 codegen trace 收集 |
| Channel/Mutex/Atomic is_linear() | 0.31.16 | 降级 known limitation | builtin 函数（整数 handle），非 ResolvedType Nominal |
| consumed_flow_vars 删除 | 0.31.16 | 降级 known limitation | 保留为诊断增强层（E0423 带 transition 名） |

- **Consumer 迁移审计**：interp/codegen/verifier 三后端声明层（签名/Flow transition/Actor/Session/Protocol/ownership/CFG）从 CheckedProgram 安装；**函数体仍经 `legacy_body_file()` 消费 raw AST**（`interp/mod.rs:323`、`codegen/compile.rs:596`、`verifier/ctx.rs:1130`、`verifier/mod.rs:49,125`）。`legacy_body_file()` 为 `pub(crate)` 可见性，阻止 crate 外新 consumer 回退。函数体 Resolved IR 迁移按 body class 追踪于 0.31.8–0.31.19。

### 0.31.17 高阶交互闭环（完成）

- **闭包 × Flow**：lambda 内引用外层 flow state 变量 → E0427 拒绝（"linear resource cannot be captured by closure"）。`lambda_depth` + `lambda_param_names` 追踪，区分参数和 capture。Lambda 参数中的 flow state 合法。
- **集合 × Flow**：`[s0, s1]` list literal → E0427 拒绝。Map literal value 为 flow state → E0427 拒绝。
- **修复既有坏测试**：`flow_state_lambda_param_accepted`（fn 类型语法不合法）、`flow_state_in_set_rejected`（set{} 语法不存在）。
- 4108 测试全绿。

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
- Component IR / ABI / Wire 不存在（排入 0.1.1 内部 Phase C）。
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
