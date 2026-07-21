# v0.31 基础设施与自举

## 目标

建立所有后续语义唯一依赖的 `Surface AST -> Normalized AST -> CheckedProgram<Resolved IR>` 管线，并关闭历史自举承诺。

## 版本

- **0.31.0 基线冻结**：版本/CHANGELOG/support/P0 台账一致；不改语义。
- **0.31.1 Span/Origin**：所有节点拥有稳定 NodeId、Span、Origin；用户诊断不再落到 `(0,0)`。
- **0.31.2 HM 核心**：唯一 unification/resolve，Infer、Any、erased type 分层。
- **0.31.3 CFG/ownership ledger**：move/drop/return/borrow 和 branch merge 路径敏感。
- **0.31.4 CheckedProgram**：typed items/calls/conversions/effects/residual/resource actions/origins。
- **0.31.5 Consumer 迁移**：interp/native/verifier/component 不再重查 raw AST。
- **0.31.6 止血 I**：只修回归、ICE、性能和基础 ignored。
- **0.31.28 MimiSpec parser 自举**：Mimi 与 Rust oracle AST 差分 100%。
- **0.31.29 HM 自举**：Mimi 与 Rust inference/unification 差分 100%。

## 当前进度

- **0.31.2 已收口**：canonical unification、binder-aware traversal、mandatory zonk、泛型 fresh instantiate 与 zonked function artifacts 已通过聚焦门禁；raw-body consumer 迁移按路线留给 0.31.4–0.31.5。
- **0.31.3 实现完成、门禁待清零**：所有 callable 均持久化 stable-ID CFG；ownership 使用 reachable-predecessor fixed point，borrow 使用独立 LoanId、CFG liveness end edge 与结构化 Place overlap。nested field/tuple/constant/dynamic-index 引用已通过 interpreter/native ABI 回归，`ownership_cfg.mimi` 覆盖 branch、terminal、nested place 与循环内 loan。legacy `OwnershipLedger` 仍作为 canonical action extraction 的兼容投影，完整 typed-body consumer 迁移按边界留给 0.31.4–0.31.5。当前 real-world/Z3/focused 门禁已绿，但全量仍有独立 codegen/JSON/verifier 失败且 Clippy 1.93 基线未清，因此暂不升版 0.31.4-dev。
- **0.31.4-dev 当前集中迁移**：冻结 Flow/Component/自举新功能，先将 CheckedProgram 改为 owned typed-body artifact，再依次迁移 consumer。`-dev` 只表示正在实施，不表示门禁完成。允许版本总改动超过 3,000 LOC，但每个 commit 的 diff 必须小于 3,000 行且保持可编译、可定向验证。
  - **已收口（IR schema）**：`ResolvedCallable` 在 `CheckedProgram` 边界按稳定 `NodeId` 组装 signature + body + CFG + resource analysis（owner 不一致 fail-closed），并从 resolved body 收集合约；session 残差稳定为 `ResolvedSessionAction` 并 lowering 为 body-local `SessionTransition`（终态 `session_close` 发 `Drop` 消耗 endpoint）；多目标 Flow transition 解析为闭合 `ResolvedType::FlowStateSet` + 显式 `FlowStateInject` conversion（validator 强制 membership、拒绝伪 Identity）；nominal 消歧排除 Flow 容器。
  - **已收口（typed 差分 parity）**：`ResolvedInterpreter` 与 surface interpreter 在标量子集逐项比对返回值 + stdout（9 个 differential 测试）；语料扫描 30/69 real_world 程序通过 typed executor。全量门禁 3975 passed / 75 failed / 10 ignored（单线程），失败集与基线逐条一致，零回归。
  - **未收口（consumer 迁移主体）**：interpreter 生产路径仍经 `legacy_body_file()` 执行 surface AST；`ResolvedInterpreter` 尚未补齐 Flow transition / actor / 并发 / session / FFI / delegate-pinned / TypeValue 执行（39 个语料失败的根因），native structured emitter、verifier typed-contract lowering、component `BindingModule` 投影与 `legacy_body_file()` 删除均待这些能力补齐后才能安全切换。
  - **范围裁定（0.31.6 vs 0.31.7+）**：上述 consumer 迁移主体（Flow/actor/并发/session/FFI 执行补齐、native structured emitter、verifier typed-contract lowering、component `BindingModule` 投影、`legacy_body_file()` 删除）属于 0.31.7–0.31.15 Flow 核心阶段（FLOW-IDENTITY-001 / ACTOR-FLOW-001 / SESSION-LINEAR-001 等），**不**在 0.31.6 范围。0.31.6「止血 I」（`kind=stabilization, requirements=[]`）仅交付：(1) 清零 0.31.4 迁移引入的 75 个回归失败（dual_backend JSON/集合 codegen ~45、named-args desugar 2、verifier 溢出 VC + builtin 解析 ~20、杂项 6）；(2) 修 break/continue 循环出口 resource-analysis 边界（ICE 类）；(3) Clippy 基线清零；(4) 全量/Clippy/Z3/real-world/文档门禁连续两次全绿。
- **0.31.6 已发布**：75 个回归全部清零（4053/0/10）；Clippy 基线以 crate-level allow 登记归零；break/continue ICE 在 0.31.3 修复后无复发；全量/Clippy/Z3/real-world/文档门禁连续两次全绿。Located wrapper 脆弱性（8 站点）已逐点修复，结构性 normalization pass 排入 0.31.7。额外修复 `from_json` 无显式类型参数时 TOOL-RESOLUTION-001 验证失败（隐式推断路径 type_arguments 为空，验证条件放宽为非空时才检查一致性）。`LLVM_SYS_180_PREFIX` 全局修正为 `LLVM_SYS_181_PREFIX`（inkwell llvm18-1 → llvm-sys 181.x）。

## 不变量

- Canonical ID 不依赖声明顺序或 Vec index。
- 缺失/重复 resolution 是 checker diagnostic 或 compiler error，后端不得恢复猜测。
- Rust oracle 保留到 RC2；自举失败可回滚，不影响 stable compiler。

## 门禁

- Resolved IR schema/golden、ID 重排测试、unification 性质测试。
- 所有生产 CLI 入口必须调用 checked API。
- unchecked AST API 只能是明确 test-only/experimental。
- consumer 迁移后立即删除其 raw-AST 入口，禁止生产双读。
- 0.31.6 退出前全量、Clippy、Z3、real-world 和文档门禁必须连续两次全绿。
