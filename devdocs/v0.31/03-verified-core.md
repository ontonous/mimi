# v0.31 Verified Core 1

> v2 更新（2026-07-25）：基于外部盲审反馈，补充 f64 opaque sort、typestate 公理注入、
> 验证域隔离、语义规范化 Hash 等设计需求。权威设计见 `devdocs/pre-1.0/03-verified-core.md`。

## 版本

- **0.31.23**：Resolved IR -> trusted subset -> Verification IR -> CFG/SSA。
- **0.31.24**：checked i32/i64、definedness obligation、VC 与 counterexample。
- **0.31.25**：八结果代数、proof artifact、solver/infrastructure fail-closed、验证域隔离。
- **0.31.26**：止血/攻击审查 II，0 新 proof feature。

## 范围

首版只允许 bool、checked i32/i64、f64 opaque sort（类型传递，算术不验证）、纯有限分支、immutable scalar 和受限 old(param)。heap、Flow、Actor、Session、loop、recursion、allocation、任意 call、spawn/await、FFI 在 SMT 前返回 `NotInTrustedSubset`。

## 0.31.23 交付（Verification IR）

### 核心交付

- `VFunction`/`VBlock`/`VExpr` 类型定义（`docs/spec/verified-core-1.md` §3）
- Resolved IR → VIR lowering（trusted-subset gate 在 lowering 前拒绝 unsupported 构造）
- CFG/SSA lowering（从 `core/cfg/` 已有基础设施扩展）
- VIR 节点**不携带 Span**（Span 存 side table，用于错误报告；§12.1 语义 Hash 前置）
- 局部变量使用 De Bruijn 索引或 canonical name（`%0`, `%1`, `%2`）

### f64 opaque sort（v2 新增）

- f64 作为 Z3 uninterpreted sort 进入 VIR（`declare-sort F64`）
- f64 参数/返回/let binding 正常传递
- f64 算术（`+`, `-`, `*`, `/`, `%`）→ `NotInTrustedSubset`
- f64 比较（`<`, `>`, `==`）→ uninterpreted predicate（不证明，不拒绝合约）
- 替换当前 `expr.rs:248-374` 的 exact Reals 编码（不健全）

### Typestate 公理注入（v2 新增）

- `VFunction` 携带 `typestate_context: Option<TypestateAxioms>`
- Flow transition VIR 生成时注入：
  - 源状态不变量 → Z3 公理（assert）
  - 转移守卫 → Z3 前置条件
  - 目标状态不变量 → Z3 义务（prove）
- 公理来源：Checker 已验证的 typestate 信息（状态 invariant、transition guard、payload 类型）
- 不得注入未经 Checker 验证的假设
- 替换当前 `verifier/flow.rs:207-226` 的 `ret=None` 合成（丢失 typestate 上下文）

### 门禁

- VIR 类型定义完整，覆盖 bool/i32/i64/f64-opaque/checked-arith/compare/boolean/select
- trusted-subset gate 在 SMT encoding 前拒绝 unsupported 构造
- f64 算术不进入 Z3 encoding（NotInTrustedSubset）
- Flow transition VIR 携带 typestate 公理
- VIR 节点无 Span（Span 在 side table）
- Raw AST 永不产生 stable Proven

## 0.31.24 交付（VC + checked integer）

### 核心交付

- Checked integer 语义：每个 i32/i64 运算生成值方程 + definedness obligation
- Division/modulo：divisor != 0 + `MIN / -1` definedness
- Verification condition 生成（从 VIR CFG/SSA）
- Counterexample 提取（int/real，映射回源码变量名）
- 与解释器/native 的整数语义一致性测试

### 门禁

- 整数模型与两后端完全一致
- 每个 partial operation 有 definedness obligation
- known-unsound corpus 误证数为 0
- 测试包含 overflow、除零、MIN/-1、2^53 等反例

## 0.31.25 交付（artifact + fail-closed + 验证域隔离）

### 核心交付

- 8 态结果代数：Proven / Disproven / NotInTrustedSubset / SolverUnknown / Timeout / InfrastructureError / RuntimeOnlyContract / NoObligations
- Proof artifact：semantics version + integer model + float model + solver/version + source hash + Resolved IR hash + VIR hash
- Z3 不可用 → InfrastructureError（不返回 mock Unknown）
- Solver crash → 当前 proof session 失败

### 验证域隔离（v2 新增）

- **合约级** NotInTrustedSubset（合约表达式含 unsupported 构造）→ `mimi verify` 错误
- **Body 级** NotInTrustedSubset（body 含 unsupported 构造，合约是标量）→ Unknown（不阻断）
- `#[verified]` 属性：Unknown 也是错误（面向关键路径：FFI 边界、XPU 调度算法）
- `mimi build` 不跑 Z3（当前已是如此，保持不变）
- `mimi build --verify-ffi` 只对 FFI call site 跑 Z3

### 语义规范化 Hash 与证明缓存（v2 新增）

- VIR hash 使用语义规范化（去 Span + 变量规范化）
- Hash 算法：BLAKE3
- 缓存 key：`(semantics_version, solver_version, integer_model, vir_hash)`
- 增量验证：只重新验证变更的函数 + 依赖链（缓存是补充）
- 缓存存储：`reports/verify-cache/` 或 `$XDG_CACHE_HOME/mimi/verify/`

### 门禁

- Artifact 绑定 semantics/model/solver/source/Resolved IR/VIR hash，支持重放和篡改检测
- Unknown/timeout/infrastructure failure 全部 fail-closed
- `build --verify-ffi` 不放行 Unknown
- 合约级 NotInTrustedSubset 阻断 `mimi verify`；body 级不阻断
- 证明缓存命中率 > 80%（变量重命名、注释变更不失效）

## 0.31.26 交付（Verification IR）

> 实际交付（roadmap.toml 权威，03-verified-core.md 原 0.31.23–26 编号已顺延）：

- VIR 类型定义：VType / VExpr / VStmt / VFunction / TypestateAxioms / VirSpanTable / VarId
- trusted-subset gate：SMT 前拒绝 heap / loop / call / mutation / let mut / 未知类型
- FuncDef → VIR lowering：canonical VarId（%0, %1, …）、span side table、infer_expr_type
- VIR → Z3 encoding：CheckedArith + definedness obligations、f64 opaque sort、F64Compare
- verify_func_vir 基础设施（未接入主验证流程）
- lower_transition_to_vir（Flow transition typestate 基础设施）
- 代码审查修复 6 项（P0×2 + P1×2 + P2×2）

### 从 0.31.26 延期的工作（显式追踪）

| 延期项 | 原因 | 目标 sprint | 验收标准 |
|--------|------|------------|---------|
| ~~**VIR 路径接入主验证流程**~~ | ~~缺少 counterexample 提取、callee ensures 传播、old() 等式约束~~ | ~~**0.31.27**~~ | ✅ 已完成（counterexample + old() + i32 range + VC artifact） |
| **Callee ensures propagation (VIR)** | trusted subset gate 拒绝 calls；需扩展 gate + VIR Call lowering | **post-0.31.27** | VIR 路径可验证含调用的函数（callee ensures 作为公理） |
| **Typestate 公理注入** | 需要 CheckedProgram 迁移（当前 verifier 用 raw AST） | **post-0.31.27** | Flow transition VIR 携带非空 TypestateAxioms，Z3 公理注入 |
| **f64 opaque sort 替换 AST 路径** | expr.rs:250-376 exact Reals 不健全，但替换影响现有测试 | **0.31.28** | f64 算术 → NotInTrustedSubset，f64 比较 → uninterpreted predicate |
| **CFG/SSA lowering** | v1 使用 tree-based Select，loop 被 gate 拒绝 | **post-0.31.28** | VIR 支持 loop + phi 节点，gate 接受有限 loop |
| **语义 Hash BLAKE3** | 当前 SipHash 非确定性跨 Rust 版本 | **post-0.31.28** | BLAKE3 替换 DefaultHasher，tamper detection 可用 |

### 0.31.27 实际交付

- **VIR 路径接入 verify_func**：trusted subset 函数优先走 VIR 路径，其余回退 AST 路径
- **Counterexample 提取**：Z3 model → 参数值 + result 值 + violated ensures 索引
- **old(param) == param 等式约束**：trusted subset 无 mutation，参数不变
- **i32 参数范围约束**：MIN <= x <= MAX，使溢出检查 sound（Z3 Int 无界）
- **f64/invariant 回退**：f64 参数/返回值、invariant 语句回退 AST 路径
- **return_vtype 上下文推断**：i32 返回类型 → 整数字面量推断为 i32（常量溢出检查）
- **VC artifact**：VIR semantics hash（SipHash）+ ProofArtifact 附加到每个 VerificationResult

## 0.31.26 原计划（攻击审查 II，已顺延）

- 验证 0.31.23-24 修复
- f64 opaque sort 不泄漏算术证明
- Typestate 公理不引入未验证假设
- 验证域隔离不被绕过（body 级 NotInTrustedSubset 不升级为 Proven）
- 语义 Hash 抗碰撞（不同语义不产生相同 hash）
- 0 新 proof feature

## 贯穿门禁

- Raw AST 永不产生 stable Proven。
- 不允许 fresh variable 隐藏 unsupported 语义。
- Unknown、timeout、solver 缺失/crash 全部 fail-closed。
- known-unsound corpus 误证数为 0。
- Artifact 绑定 semantics/model/solver/source/Resolved IR/VIR hash，并支持重放和篡改检测。
