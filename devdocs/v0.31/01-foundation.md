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

## 不变量

- Canonical ID 不依赖声明顺序或 Vec index。
- 缺失/重复 resolution 是 checker diagnostic 或 compiler error，后端不得恢复猜测。
- Rust oracle 保留到 RC2；自举失败可回滚，不影响 stable compiler。

## 门禁

- Resolved IR schema/golden、ID 重排测试、unification 性质测试。
- 所有生产 CLI 入口必须调用 checked API。
- unchecked AST API 只能是明确 test-only/experimental。
