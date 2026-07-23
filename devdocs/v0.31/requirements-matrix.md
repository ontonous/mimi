# v0.31 Requirement 分配矩阵（v2）

> 机器可读来源：`roadmap.toml`（schema_version = 2）。成熟度来源：`docs/language-support.toml`。
> v2 变更：插入 0.31.16–18（地基深修）、0.31.27（Component 稳定检查点）、0.31.33（SDK 加固）、0.31.34（自举 spike）。原 0.31.16–37 重编号为 0.31.19–43。

| 版本 | Requirement | 备注 |
|---|---|---|
| 0.31.0 | TOOL-SUPPORT-001 | |
| 0.31.1 | LANG-ATTRIBUTE-001 | |
| 0.31.2 | TOOL-RESOLUTION-001 | |
| 0.31.3 | RESOURCE-LINEAR-001, OWN-PERMISSION-001 | |
| 0.31.4–0.31.5 | TOOL-RESOLUTION-001 | |
| 0.31.8 | FLOW-IDENTITY-001 | |
| 0.31.9 | FLOW-TURN-001, ERROR-PROP-001 | |
| 0.31.10 | FLOW-SPARSE-001, FLOW-FAULT-001, FLOW-PROGRESSIVE-001 | |
| 0.31.11 | ACTOR-FLOW-001 | |
| 0.31.12 | SESSION-LINEAR-001 | |
| 0.31.13 | RESOURCE-LINEAR-001, OWN-PERMISSION-001 | **追加 A**：Flow 状态 is_linear + 别名追踪 + shared 拒绝 |
| 0.31.14 | PROTOCOL-STATIC-001, PROTOCOL-DYN-001 | **追加 A**：Protocol conformance × 线性检查 |
| 0.31.15 | —（evidence） | **追加 A**：trace 所有权边 + generation 失效记录 |
| **0.31.16** | **FLOW-IDENTITY-001, RESOURCE-LINEAR-001** | **新增**：类型级线性 + generation + 删除 HashMap |
| **0.31.17** | **RESOURCE-LINEAR-001, FLOW-IDENTITY-001** | **新增**：泛型/闭包/集合 × Flow 高阶交互 |
| **0.31.18** | —（stabilization） | **新增**：证据同步 + 回归扫描 |
| 0.31.19 | —（audit） | 攻击审查 I（原 0.31.16，基于闭环地基） |
| 0.31.20 | ERROR-ALGEBRA-001, LANG-FUNCTION-001, LANG-CONTRACT-001, COMPTIME-PURE-001, LANG-ATTRIBUTE-001, SYNTAX-REMOVED-001 | 原 0.31.17 |
| 0.31.21–0.31.23 | VERIFY-CORE-001 | 原 0.31.18–20 |
| 0.31.24 | —（audit） | Verifier 止血 + 审查 II（原 0.31.21） |
| 0.31.25 | COMPONENT-IR-001, COMPONENT-RAW-001 | 原 0.31.22 |
| 0.31.26 | COMPONENT-HANDLE-001 | 原 0.31.23 |
| **0.31.27** | —（stabilization） | **新增**：Component 稳定检查点（ABI fuzz + handle race） |
| 0.31.28 | COMPONENT-CALLBACK-001, COMPONENT-ASYNC-001 | 原 0.31.24 |
| 0.31.29 | COMPONENT-WIRE-001 | 原 0.31.25 |
| 0.31.30 | — | Rust Safe SDK（原 0.31.26） |
| 0.31.31 | MULTILANG-AUTHORITY-001 | XPU FFI 验证（原 TS GUI SDK，替换为真实 C 库 E2E） |
| 0.31.32 | —（audit） | Component 攻击审查（原 0.31.28） |
| **0.31.33** | —（stabilization） | **新增**：SDK conformance 加固（双 SDK E2E + Wire fuzz） |
| **0.31.34** | —（deferred） | ~~自举可行性 spike~~（deferred to post-1.0） |
| 0.31.35 | —（deferred） | ~~MimiSpec parser 自举~~（deferred to post-1.0） |
| 0.31.36 | —（deferred） | ~~HM 自举闭环~~（deferred to post-1.0） |
| 0.31.37 | MIGRATION-PRE1-001, SYNTAX-REMOVED-001 | 原 0.31.31 |
| 0.31.38 | TOOL-SUPPORT-001 | 原 0.31.32 |
| 0.31.39 | FLOW-MULTI-001, PROTOCOL-DYN-001, EFFECT-CAP-001, COMPONENT-RAW-001 | 原 0.31.33 |
| 0.31.40 | —（debug） | DEBUG 周期（原 0.31.34） |
| 0.31.41 | —（audit） | 最终敌对审查（原 0.31.35） |
| 0.31.42 | —（rc） | RC1（原 0.31.36） |
| 0.31.43 | —（rc） | RC2 与 Pre-1.0 退出（原 0.31.37） |

## 版本类型统计

| 类型 | 数量 | 版本 |
|------|------|------|
| implementation | 27 | 0.31.1–5, 8–17, 20–23, 25–26, 28–31, 35–39 |
| stabilization | 5 | 0.31.6, 18, 27, 33 + 止血 II (0.31.7) |
| audit | 5 | 0.31.19, 24, 32, 41 + Component 审查 |
| evidence | 1 | 0.31.15 |
| spike | 1 | 0.31.34 |
| debug | 1 | 0.31.40 |
| rc | 2 | 0.31.42, 43 |
| baseline | 1 | 0.31.0 |

## 关键依赖链（v2）

```
0.31.13 追加 A（is_linear 谓词 + 别名追踪）
    ↓
0.31.16（类型级线性 + generation + 删除 HashMap）
    ↓
0.31.17（高阶交互：泛型/闭包/集合）
    ↓
0.31.18（证据同步）
    ↓
0.31.19（攻击审查 I）── Phase B 启动门
    ↓
0.31.20–24（语言冻结 + Verified Core）
    ↓
0.31.25–26（Component IR + ABI）
    ↓
0.31.27（稳定检查点）── Callback/Wire 启动门
    ↓
0.31.28–32（Callback + Wire + SDK + 审查）
    ↓
0.31.33（SDK 加固）── Phase D 启动门
    ↓
0.31.34–36（自举，DEFERRED to post-1.0，不阻塞）
    ↓
0.31.37–39（迁移 + 工具 + 隔离）
    ↓
0.31.40–43（DEBUG + 审查 + RC1 + RC2）
```
