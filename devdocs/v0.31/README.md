# Mimi v0.31.x 路线图

> 状态：权威实施路线图
> 目标范围：完整覆盖 Mimi Pre-1.0
> 规范来源：`docs/language-spec.md`、`docs/spec/`、`docs/language-requirements.toml`
> 机器索引：`roadmap.toml`

## 1. 定位

v0.30.0 是已完成的止血基线。v0.31.x 不重做 v0.30 已关闭的架构债务，而是在其上建立唯一语义中枢，并完整实现 Pre-1.0 的 31 项 requirement。

v0.31.x 的完成不由版本号决定。即使到达 v0.31.37，只要 stable requirement、P0、双后端轨迹、Verified Core、Component conformance、迁移或 RC 门禁有一项未满足，就继续增加 v0.31.x。

## 2. 权威边界

1. `docs/language-spec.md` 和 `docs/spec/` 定义语义。
2. `docs/language-requirements.toml` 定义 requirement ID、目标状态和 gate。
3. 本目录定义版本顺序、依赖、预算和证据，不改变语义。
4. `docs/language-support.toml` 记录当前实现成熟度。
5. `reports/` 记录每版证据，不具有规范权威。

## 3. 依赖主链

```text
Span/Origin -> HM -> CFG/ownership -> CheckedProgram/Resolved IR
  -> Flow generation/turn -> Actor/Session/resource -> semantic trace
  -> Verified Core
  -> Component IR -> Native ABI -> Wire -> Rust/TS SDK
  -> self-hosting/migration/tooling -> DEBUG/audit -> RC1 -> RC2
```

任何下游不得绕过上游：后端不能重查 AST，verifier 不能从 raw AST 产生 Proven，binding generator 不能绕过 Component IR。

## 4. 版本阶段

| 阶段 | 版本 | 主题 |
|---|---|---|
| 基础设施 | 0.31.0–0.31.7 | 基线、Span、HM、CFG、Resolved IR、consumer 迁移、止血 I/II |
| Flow 核心 | 0.31.8–0.31.16 | generation、atomic turn、Fault、Actor、Session、资源、Protocol、Trace、审查 |
| 语言与验证 | 0.31.17–0.31.21 | 语法收敛、Verification IR、VC、artifact、攻击审查 |
| Component | 0.31.22–0.31.28 | Component IR、ABI、callback/async、Wire、Rust/TS SDK、审查 |
| 自举与工具 | 0.31.29–0.31.33 | MimiSpec/HM 自举、迁移、fmt/LSP/probes、experimental 隔离 |
| 冻结 | 0.31.34–0.31.37 | DEBUG、最终敌对审查、RC1、RC2 |

详细版本及 requirement 分配见 `roadmap.toml` 和 `requirements-matrix.md`。

## 5. 状态模型

版本状态：`planned -> implementing -> evidence_pending -> complete`。

异常状态：`blocked`、`rolled_back`、`superseded`。

只有以下条件同时满足才能标记 complete：

- 本版声明的 requirement 切片有自动 probe；
- 适用的 S/L1/L3/V/C/T/A 门禁通过；
- support evidence 已更新且不高于实测；
- 无新增 P0、silent fallback、warning-only stable unsupported 或 ignored；
- 目标测试连续两次通过。

## 6. 变更预算

- 普通实现版：建议净新增生产代码不超过 3,000 LOC，最多 2 个新核心抽象。
- 止血、DEBUG、审查、RC：0 个新 stable feature。
- 每版预留 25% 容量修复回归。
- 超预算必须拆版，不以“临时兼容层”掩盖未完成迁移。

## 7. 贯穿止血线

任一版本发现以下问题即阻断当前版本：

- checker 接受而后端 warning/no-op/首候选/首目标/sentinel 降级；
- unresolved type、Session residual 或资源 ownership 进入后端；
- raw AST 或 unsupported node 获得 Proven；
- 裸整数 handle、ABA、wrong runtime/kind、lookup/destroy TOCTOU；
- callback/cancel 无 quiescence 或出现两个 terminal outcome；
- 测试绕过 CheckedProgram 却被计入 stable evidence；
- 新增无契约 unsafe、panic/unwrap、silent error 或 ignored test。

## 8. 分册

- `01-foundation.md`：0.31.0–0.31.7、0.31.29–0.31.30
- `02-flow-runtime.md`：0.31.8–0.31.16
- `03-verified-core.md`：0.31.18–0.31.21
- `04-component-boundary.md`：0.31.22–0.31.28
- `05-migration-tooling.md`：0.31.17、0.31.31–0.31.33
- `06-audit-debug-rc.md`：止血、审查、DEBUG 和 RC 门禁

## 9. 最终退出

- 31/31 requirement 有自动 probe 和持久 evidence；26 stable 全部 complete。
- 4 experimental 完整隔离并 fail-closed；1 removed 主路径拒绝且迁移幂等。
- Interpreter/native/verifier/component 只消费真实 Typed Resolved IR。
- Flow generation、Actor runs Flow、typed Session、resource exactly-once 闭环。
- Verified Core known-unsound 误证为 0。
- Component IR、Native ABI 1、Wire Schema 1 和 Rust/TS SDK conformance 全绿。
- MimiSpec parser 与 HM 自举差分为 0。
- P0=0，连续两个干净环境 RC 通过全部适用门禁。
