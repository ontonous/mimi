# v0.31 Flow、Actor、Session 与资源

## 版本

- **0.31.8**：nominal Flow identity、linear generation、state unforgeability。
- **0.31.9**：draft + Become/Stay/Fault/Rejected 原子 turn，transition `?` 归还 source。
- **0.31.10**：稀疏图、per-Flow typed Fault、显式 reset/recover、progressive Main 真 lowering。
- **0.31.11**：Actor runs Flow，移除稳定 Actor mutable business field。
- **0.31.12**：typed dual Session residual，覆盖 alias/field/return/branch/close。
- **0.31.13**：transition/Fault/Session resource exactly-once 与闭包 ownership。
  - **追加 A**（版本内补充，不修改已有交付）：
    - Flow 状态纳入 `is_linear()` 分类（`resource_lower.rs` 谓词扩展，覆盖 `ResolvedType::FlowStateSet` 和 Flow state nominal record）
    - Flow 状态别名追踪（对标 session E0426 机制：`let b = s0` 转移消费，原变量不可用）
    - 删除 `consumed_flow_vars.remove(name)` 的 shadowing 清除逻辑（shadowing 不重置线性）
    - 负测试：`let b = s0; Flow::inc(s0); use(b)` → 静态拒绝
    - `shared`/`local_shared`/`weak`/`&T` 包装 Flow 状态 → 编译错误
- **0.31.14**：static Protocol stable；dynamic VTable 保持 experimental。
  - **追加 A**（版本内补充）：
    - Protocol conformance 通过 CheckedProgram 验证（非 raw AST 结构匹配）
    - Protocol state payload 线性检查（conformance 验证时确认 payload 字段不被别名逃逸）
    - 负测试：protocol impl 中通过别名绕过状态消费 → 拒绝
- **0.31.15**：canonical semantic trace 和 happens-before DAG comparator。
  - **追加 A**（版本内补充）：
    - Trace 记录线性违规事件（use-after-move 尝试在 trace 中可见，即使被静态拒绝也记录诊断路径）
    - Trace 记录 generation 转移时刻（旧 generation 失效的精确位置）
    - happens-before DAG 包含 Flow 状态所有权边（ownership transfer 作为偏序关系节点）

## 后续（地基深修，详见 `02b-foundation-repair.md`）

- **0.31.16**：Flow generation 与类型级线性（结构性变更：Flow 状态类型表示升级）
  - **【v2 盲审】Mutex 线性 token API + Generation 混合模式 + Channel/Mutex/Atomic 纳入 is_linear()**
- **0.31.17**：高阶交互闭环（泛型 × 闭包 × 集合 × Flow）
  - **【v2 盲审】禁止 Flow 状态字段嵌套 Flow（可判定性底线）**
- **0.31.18**：证据同步与回归扫描（stabilization，0 新 feature）
- **0.31.19**：攻击审查 I（基于闭环地基的敌对测试）

## 核心门禁

- 旧 generation 使用、状态伪造、未声明业务边全部静态拒绝。
- 四 terminal outcome 的 interp/native trace 一致。
- 非 end endpoint 不得静默离开 scope。
- Fault/kill/timeout 后资源 exactly-once，L3 零 UAF/double free/leak。
- Actor/Session 并发 trace 采用**终端观测等价**（v2 盲审修正：same final persistent state + output multiset，非轨迹等价）。
- **（0.31.13 追加）** Flow 状态别名、shared 包装、shadowing 重置全部静态拒绝。
- **（0.31.16）** `let b = s0; Flow::inc(s0); use(b)` 是编译错误，不是 warning。
- **（0.31.17）** `identity(state)`、`fn() { Flow::inc(s0) }`、`[s0, s1]` 全部静态拒绝或转移。
- **（0.31.16 v2 盲审）** Channel/Mutex/Atomic 是线性资源：`let b = ch; channel_send(ch, v)` → 编译错误。
- **（0.31.16 v2 盲审）** Mutex lock 返回线性 token：不 unlock → 编译错误（线性资源未消费）。
- **（0.31.16 v2 盲审）** Generation 同线程同步零开销；跨 Actor/FFI 边界打包校验。
- **（0.31.17 v2 盲审）** Flow 状态字段禁止嵌套 Flow：`flow Pool<T> where T: Flow` → 编译错误。
