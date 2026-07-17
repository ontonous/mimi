# Mimi Verified Core 1

> Normative profile: `mimi-verified-core-1`
> Binding source: `docs/language-requirements.toml`. This header is descriptive; the manifest is authoritative.

## 1. 允许集合

- 类型：`bool`、checked `i32`、checked `i64`；
- 值：字面量、immutable scalar 参数、局部 immutable binding；
- 控制流：有限且穷尽的 if/match，经 CFG/SSA lowering；
- 合约：pure/total specification expression 和 `old(immutable_scalar_parameter)`；
- 调用：首版禁止；未来只允许已 Proven 的 pure total acyclic summary。

f32/f64、heap、aggregate alias、mutation、loop、recursion、allocation、panic、I/O、time、random、FFI、Flow、Actor、Session、spawn/await 和未知类型在 SMT encoding 前返回 `NotInTrustedSubset`。

## 2. Checked Integer

每个算术操作同时生成值方程和 definedness obligation。加减乘检查目标位宽；除模检查 divisor != 0 与 `MIN / -1`。解释器、native、constant folding、comptime 和 verifier 使用同一规则。Wrapping 只能由显式 wrapping operation 请求。

## 3. Verification IR

```text
VFunction { id, params, blocks, entry, postconditions, semantics_hash }
VBlock    { phis, statements, terminator }
VExpr     = Const | Var | CheckedArith | Compare | Boolean | Select
```

不允许用 fresh variable 代替 unsupported node。Raw AST 入口只能用于标记为 experimental 的 encoder test，不能产生 stable `Proven`。

## 4. 结果

`Proven`、`Disproven(counterexample)`、`NotInTrustedSubset(node)`、`SolverUnknown`、`Timeout`、`InfrastructureError`、`RuntimeOnlyContract`、`NoObligations`。

证明门禁只接受 `Proven` 或 `NoObligations`。solver 缺失、crash、Unknown 和 timeout 均 fail-closed。

## 5. Artifact

Artifact 必须记录 profile、integer model、float/heap/call model、solver/version、source hash、Resolved IR hash、Verification IR hash、obligation 和结果。证明不默认删除 runtime contract；删除检查需要独立、绑定 artifact 的优化 profile。
