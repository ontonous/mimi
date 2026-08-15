# 0.1.6 已知边界 / 已知延后清单

> **状态**：0.1.6 终测（0.36.114）固化版。
> **范围**：本节只记录“已确认不阻塞 0.1.6 里程碑、但正式延后到 Wave-3 / 0.2 /
> 1.x”的边界。0.1.6 内已按设计闭合的审计项见 CHANGELOG 0.36.56–114。
> **机器清单**：边缘项以 `devdocs/v0.36/edge-inventory.toml` 为权威；
> 门禁 `scripts/check_edge_isolation.py` 强制核心门禁零边缘依赖。

---

## 1. Wave-3 结构项（正式固化，不作为 0.1.6 阻塞）

以下为长期结构性工程目标。0.1.6 已把其中若干条“按设计闭合”为有明确缓解/
当前交付面，但完整架构目标仍延后到 Wave-3：

| 项 | 0.1.6 现状（已核验） | 后续目标 |
|----|---------------------|----------|
| 三引擎逐特性等价矩阵（legacy / resolved / VM） | 已有 `dual_backend.rs` 全量 VM↔codegen 差分 + resolved IR 统一前端；legacy 不是 0.1.6 交付面（0.36.103） | Wave-3 基建 |
| bindgen 完全迁移到 Component IR | `mimi bindgen` 已先经 `checked_component_input` 并统一消费 resolved extern/type 表；7 后端不各自裸解析 AST（0.36.102） | Wave-3 架构目标 |
| Wire schema 接入 CLI/传输层 | `src/component/wire.rs` 编解码/错误路径已有实现与测试；未接 CLI/传输层（0.36.104） | Wave-3 集成目标 |
| 列表所有权模型 / struct box 长期存活泄漏 | 现有行为是有意折衷：直接注册会造成 UAF/double-free；进程退出回收（0.36.105） | Wave-3 列表所有权重构 |
| Go 回调 ABI user-data 根本解 | per-slot mutex 快照 + 全局 store/deregister 缓解已覆盖并发覆盖（0.36.106） | Wave-3 ABI 根解 |
| spawn/await 真实并发调度（`Op::Spawn` 发射、调度器/生命周期） | 当前编译为顺序求值，行为确定且非安全/正确性阻塞（0.36.86） | Wave-3 并发运行时结构项 |
| stdlib JSON parser 与 serde 完全统一（结构替换） | 已知分歧已逐项关闭（前导零、溢出指数、控制字符等）；保留结构替换目标（0.36.84/0.36.85 附近台账） | Wave-3 结构项 |

## 2. 0.2 事项（明确延后）

| 项 | 说明 |
|----|------|
| rich fault variant 集（`fault F { A \| B }` 变体块） | Phase A 已按 nominal 单错误闭环；变体块排 0.2（spec §3.12、edge-inventory fault-variants） |
| Component IR / Wire Schema 广度扩张 | Wire 编解码已实现，传输/组件体系全面扩张排 0.2 |
| `ffi slice` / `ffi slice_mut` / `ffi buffer` 纸面特性 | parser/codegen 零支持，与指针读写同属 0.2 Component IR 面（language-spec §14） |
| 原生 dsp ≤1.15×、RUN dsp ≤1s 性能目标 | 0.1.5 已登记 out-of-scope，0.1.6 未重新承诺（README 0.1.5 行） |
| ABI 版本握手实施（native-abi-1 §8） | 0.2 随 bindgen 回归铁律一并实施 |

## 3. 1.x 评估项（fail-closed 保留）

| 项 | 当前处置 |
|----|----------|
| `T?` nullable 类型后缀 / 其它已删语法 | 已删除并由迁移诊断/语法门禁防止回潮；1.x 不因语法回顾开放 |
| match guard 精确 true/false CFG 分叉（G-3） | 当前按恒真建模，消费高估 = fail-closed（0.36.85 按设计闭合） |
| i32/i64 宽度模型统一（A1 残留 i32 folding） | 已按宽度 trap 自洽定案；1.x 单态化时一并裁决 |
| 索引/切片/元组投影的线性容器非定向提取 | 0.1.6 已开放 `let c = xs[0]` 定向头提取；其余保持 E0304/E0432 fail-closed |
| 线性容器提前退出路径的逐元素记账 | for 提前退出仍保持 fail-closed（E0256）；完整逐元素 Drop 记账为后续切片 |
| verifier 双引擎整数模型统一 | 0.1.6 保持 E0439 fail-closed 显式分歧；统一为 0.2 退役前置条件 |

## 4. 边缘隔离项（不阻塞核心门禁）

机器权威清单：`devdocs/v0.36/edge-inventory.toml`。当前 6 项：

- Effect lattice（with 已废，lattice 未冻结）
- Protocol 动态分发（dyn/VTable/异构集合）
- recursive/multiparty/delegation Session
- rich fault variant 集
- Component IR / Wire Schema 扩张
- comptime/quote 高级形态

每项均满足：`core_dep = false`、测试 `#[ignore = "EDGE-GATE:<marker>: …"]`、
源码注释 `// EDGE-GATE:<marker>`，门禁 `check_edge_isolation.py` 全绿。

## 5. 结论

0.1.6 里程碑不包含上述项；它们已被正式固化到 Wave-3 / 0.2 / 1.x 路线，
不再作为 0.1.6 的“活缺口”参与门禁判定。后续进入 Wave-3 时按本清单逐项
重新评估优先级，并从通道独立 gate 转正。
