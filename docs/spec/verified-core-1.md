# Mimi Verified Core 1

> Normative profile: `mimi-verified-core-1`
> Binding source: `docs/language-requirements.toml`. This header is descriptive; the manifest is authoritative.
> v3 澄清：Verified Core 1 的允许类型只有 bool、checked i32、checked i64；f64、FFI、loop 与 aggregate proof 不属于此 profile。MIR 专用 FFI 实验 profile 见 `docs/language-spec.md` §5.11。

## 1. 允许集合

- 类型：`bool`、checked `i32`、checked `i64`；
- 值：字面量、immutable scalar 参数、局部 immutable binding；
- 控制流：有限且穷尽的 if/match，经 CFG/SSA lowering；
- 合约：pure/total specification expression 和 `old(immutable_scalar_parameter)`；
- 调用：首版禁止；未来只允许已 Proven 的 pure total acyclic summary。

`f32/f64`、heap、aggregate alias、mutation、loop、recursion、allocation、panic、I/O、time、random、FFI、Flow、Actor、Session、spawn/await 和未知类型在 SMT encoding 前返回 `NotInTrustedSubset`。本规则不限制带有独立 profile、model 与 artifact identity 的实验性 verifier。

## 2. Checked Integer

每个算术操作同时生成值方程和 definedness obligation。加减乘检查目标位宽；除模检查 divisor != 0 与 `MIN / -1`。解释器、native、constant folding、comptime 和 verifier 使用同一规则。Wrapping 只能由显式 wrapping operation 请求。

## 3. Verification IR

```text
VFunction { id, params, blocks, entry, postconditions, semantics_hash, typestate_context }
VBlock    { phis, statements, terminator }
VExpr     = Const | Var | CheckedArith | Compare | Boolean | Select | OpaqueF64
```

- VIR 节点不携带 Span（Span 存 side table，用于错误报告）；
- 局部变量使用 De Bruijn 索引或 canonical name（`%0`, `%1`, `%2`）；
- `typestate_context`：Flow transition 的源状态不变量（公理）、转移守卫（前置条件）、目标状态不变量（义务）；
- 不允许用 fresh variable 代替 unsupported node。Raw AST 入口只能用于标记为 experimental 的 encoder test，不能产生 stable `Proven`。

## 4. 结果

`Proven`、`Disproven(counterexample)`、`NotInTrustedSubset(node)`、`SolverUnknown`、`Timeout`、`InfrastructureError`、`RuntimeOnlyContract`、`NoObligations`。

证明门禁只接受 `Proven` 或 `NoObligations`。solver 缺失、crash、Unknown 和 timeout 均 fail-closed。

### 4.1 验证域隔离

NotInTrustedSubset 的阻断行为区分合约级和 body 级：

- **合约表达式**中的 NotInTrustedSubset → `mimi verify` 错误（合约本身不可验证）；
- **函数 body** 中的 NotInTrustedSubset → Unknown（不阻断，除非 `#[verified]`）；
- `mimi build` 不跑 Z3；
- `#[verified]` 属性：所有义务必须 Proven，Unknown 也是错误。

## 5. Artifact

Artifact 必须记录 `semantics_version`、`proof_profile`、`obligation_scope`、integer model、float model、heap/call model、solver/version、source hash、Resolved IR hash、Verification IR hash、obligation 和结果。当前 artifact semantics version 为 3；旧版本不与当前 artifact 兼容。Verified Core 1 的 `proof_profile` 为 `mimi-verified-core-1`，不得用 MIR FFI 实验结果填充或别名此字段。证明不默认删除 runtime contract；删除检查需要独立、绑定 artifact 的优化 profile。

### 5.1 语义规范化 Hash

VIR hash 使用语义规范化：去 Span、局部变量规范化（De Bruijn / canonical name）。Hash 算法 BLAKE3。缓存 key 至少包含 `(semantics_version, proof_profile, obligation_scope, solver_version, integer_model, float_model, engine, program_identity)`；MIR 结果还绑定 route receipt identity。增量验证（变更函数 + 依赖链）优先于缓存。Requires-only 与 Requires+Ensures 是不同 obligation scope，不能互用缓存。
