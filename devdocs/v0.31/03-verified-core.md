# v0.31 Verified Core 1

## 版本

- **0.31.22**：Resolved IR -> trusted subset -> Verification IR -> CFG/SSA。
- **0.31.23**：checked i32/i64、definedness obligation、VC 与 counterexample。
- **0.31.24**：八结果代数、proof artifact、solver/infrastructure fail-closed。
- **0.31.25**：止血/攻击审查 II，0 新 proof feature。

## 范围

首版只允许 bool、checked i32/i64、纯有限分支、immutable scalar 和受限 old(param)。float、heap、Flow、Actor、Session、loop、recursion、allocation、任意 call、spawn/await、FFI 在 SMT 前返回 `NotInTrustedSubset`。

## 门禁

- Raw AST 永不产生 stable Proven。
- 不允许 fresh variable 隐藏 unsupported 语义。
- Unknown、timeout、solver 缺失/crash 全部 fail-closed。
- known-unsound corpus 误证数为 0。
- Artifact 绑定 semantics/model/solver/source/Resolved IR/VIR hash，并支持重放和篡改检测。
