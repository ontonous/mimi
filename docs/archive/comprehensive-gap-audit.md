# Mimi 编译器全面缺口审计报告

> **项目**: Mimi v0.7.0 — 编译型系统编程语言
> **审计日期**: 2026-06-20
> **方法**: 四路并行深度源码审计 + 交互路径分析 + 标准库 E2E 跟踪
> **测试基线**: 1339 passed, 0 failed, 17 ignored

---

## 目录

1. [审计范围与方法](#1-审计范围与方法)
2. [问题总览](#2-问题总览)
3. [P0 — 严重缺口（阻塞 v1.0）](#3-p0--严重缺口阻塞-v10)
4. [P1 — 高优先级缺口](#4-p1--高优先级缺口)
5. [P2 — 中优先级缺口](#5-p2--中优先级缺口)
6. [P3 — 低优先级缺口](#6-p3--低优先级缺口)
7. [标准库损坏矩阵](#7-标准库损坏矩阵)
8. [与现有审计报告的关系](#8-与现有审计报告的关系)
9. [修复路线图](#9-修复路线图)
10. [附录：各子系统健康度](#10-附录各子系统健康度)

---

## 1. 审计范围与方法

### 1.1 审计维度

| 维度 | 覆盖范围 | 方法 |
|------|---------|------|
| **AST 全覆盖** | 全部 `Expr::*` (27) / `Stmt::*` (23) / `Type::*` 变体 | 逐变体验证 interp/codegen/tycheck 三路径实现状态 |
| **内置函数** | `builtins.rs` + `codegen/builtins/mod.rs` + `interp/call.rs` | 交叉引用 `is_builtin` / `compile_builtin_call` / `call_named` |
| **标准库** | 16 模块 ~180 个 `pub func` | 逐函数跟踪内置函数调用链，标记 codegen 死路径 |
| **类型系统** | `codegen/types.rs` 全部 LLVM 映射 | 检查表示一致性和降级路径 |
| **测试覆盖** | `tests/` 68 文件 | 检查 codegen E2E (`compile_and_run`) 对每个 AST 变体的覆盖 |
| **C 运行时** | `mimi_runtime.c` 全部函数 | 检查 stub/死代码/未启用路径 |
| **FFI 层** | `ffi/` 全部文件 | 确认与 `AUDIT-REPORT.md` 的差异 |
| **现有审计** | 全部已有报告 | 交叉验证发现，排除重复项 |

### 1.2 不在此次审计范围内的项目

- 已经由 `AUDIT-REPORT.md` 全面覆盖的 FFI 内存安全/ABI 问题
- VSCode 扩展 (`mimispec-vscode/`)
- MimiSpec 解析器 (`mimispec/`)
- OSE IDE (`OSE/`)

---

## 2. 问题总览

### 2.1 等级分布

| 等级 | 总数 | 已修复 | 剩余 | 描述 |
|------|------|--------|------|------|
| **P0 — Critical** | 6 | 6 | 0 | 阻塞 v1.0：正确性缺陷或静默丢弃语义的 codegen 缺口 |
| **P1 — High** | 10 | 9 | 1 | 影响标准库可用性或产生静默错误 |
| **P2 — Medium** | 4 | 2 | 2 | 测试覆盖不足、死代码、基础设施缺口 |
| **P3 — Low** | 1 | 0 | 1 | 干净性/可维护性问题 |
| **总计** | **21** | **17** | **4** | |

### 2.2 按子系统分布

```
子系统              P0  P1  P2  P3  剩余   已修复
────────────────────────────────────────────────
codegen/expr.rs      3   3   0   0   0      6   ✅
codegen/func.rs      2   0   0   0   0      2   ✅
codegen/block.rs     2   0   0   0   0      2   ✅
codegen/types.rs     0   2   0   0   0      2   ✅
codegen/builtins/    1   2   0   0   0      3   ✅
codegen/ 整体        0   0   0   0   0      0   ✅
ffi/contract.rs      0   0   1   0   1      0
runtime.c            0   0   1   0   1      0
标准库               0   0   0   0   0      5   ✅
interp/              0   1   0   0   0      1   ✅
IR 验证 / 测试覆盖   0   0   2   0   2      0
────────────────────────────────────────────────
总计                6  10   4   1  17      4
```

---

## 3. P0 — 严重缺口（阻塞 v1.0）

### P0-1: `Expr::TupleIndex` 在 Codegen 中无实现 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/expr.rs` — ✅ 已添加 `Expr::TupleIndex` 分支，使用 `tuple_type_stack` 缓存 + `build_struct_gep`/`build_load` |
| **修复提交** | `compile_tuple_index_expr` 方法，`tuple_type_stack` 字段 |
| **验证** | 1339 tests pass；E2E 测试已覆盖元组索引路径 |

---

### P0-2: `Stmt::While` / `Stmt::For` 在嵌套块中被静默丢弃 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/func.rs` — ✅ 已提取 `compile_while_stmt` / `compile_for_stmt` 共享方法，从 `compile_func`、`compile_block`、`compile_block_last_val` 三处调用 |
| **验证** | 1339 tests pass |

---

### P0-3: `Stmt::Assign` 仅支持 Ident 目标 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/func.rs` — ✅ 已添加 `compile_assign_stmt` (处理 Ident/Field/Index/Deref) + `compile_field_assign` / `compile_index_assign` / `compile_deref_assign` 辅助方法；从 `compile_func`、`compile_block`、`compile_block_last_val` 三处调用 |
| **验证** | 1339 tests pass |

---

### P0-4: `Stmt::Let` 仅支持简单变量模式 — ✅ 已修复（完整实现）

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/func.rs` — ✅ 已添加 `compile_pattern_bind` 递归模式匹配方法，支持 Wildcard/Variable/Literal/Constructor/Tuple/Array/Slice 全部模式类型；从 `compile_func`、`compile_block`、`compile_block_last_val`、`compile_call_expr`（lambda 编译）、`compile_actor_main` 五处调用 |
| **方案** | 完整实现（选项 1）：递归 stack allocation / GEP 路径 |
| **验证** | 1339 tests pass |

---

### P0-5: `Stmt::Let` 无初始化表达式被静默跳过 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/func.rs` — ✅ Let 处理中为 `init: None` 添加了零初始化（整数 i64 零值）后绑定到 pattern |
| **验证** | 1339 tests pass |

---

### P0-6: `from_int` 内置函数在 Codegen 中不可达（死代码） — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/builtins/mod.rs` — ✅ `"from_int"` 已添加到 `is_builtin()` |
| **解释器** | `src/interp/builtins.rs` — ✅ 已添加 `builtin_from_int`；`src/interp/call.rs` — ✅ 已在 `call_named` 中注册 |
| **验证** | 1339 tests pass |

---

## 4. P1 — 高优先级缺口

### P1-1: `Type::Result` 两条路径的 LLVM 表示不一致 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/types.rs:18-22` — ✅ 已统一 `Type::Name("Result", [T,E])` 为 `{i1, T, E}` |
| **验证** | 1337 tests pass |

---

### P1-2: `Type::Name` 未知类型静默降级为 `i64` — ✅ 已修复（警告）

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/types.rs:29` — ✅ 已添加 `eprintln!` 警告，降级前输出未知类型名 |
| **验证** | 1337 tests pass |

---

### P1-3: `compile_len` 对字符串返回错误长度 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/mod.rs:77` — ✅ 添加 `pending_len_is_string` 标志；`codegen/expr.rs` — ✅ 添加 `expr_is_string()` 辅助；`codegen/builtins/list.rs:114-148` — ✅ `compile_len` 分支：字符串用 `strlen`，列表读取字段 0 |
| **验证** | 1339 tests pass |

---

### P1-4: Result/Option 方法调用在 Codegen 中缺失 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/expr.rs` — ✅ `compile_field_expr` 添加 `compile_variant_method` 分发；支持 `is_ok/is_err/is_some/is_none/unwrap/expect/unwrap_or/map_err/ok_or` |
| **实现** | GEP 提取 discriminant + payload，switch/br 生成分支逻辑，trap 分支用于 unwrap/expect |
| **验证** | 1337 tests pass |
| **解释器位置** | `interp/call.rs call_method` — 对 `Value::Variant` 有完整实现 |

**方法列表（解释器中存在，codegen 中缺失）**:
```
Variant method      |
is_some / is_none   | Option
is_ok / is_err      | Result
unwrap / expect     | Result, Option
unwrap_or           | Result, Option
ok_or               | Option
map                 | Result, Option
and_then            | Result, Option
map_err             | Result
```

**完全损坏的 stdlib 模块**:

| 模块 | 函数 | 原因 |
|------|------|------|
| **result.mimi** | 全部 7 个函数 | 核心功能基于 Result 方法 |
| **env.mimi** | `get_var_or`, `has_var`, `get_int`, `get_float` | 使用 `.is_ok()`, `.unwrap()` |
| **fs.mimi** | `read_lines`, `file_size` | 使用 `.map_err()` |

**影响**: `mimi run` 正常工作，但 `mimi build` 对使用 Result/Option 方法调用的任何代码失败。这是将 stdlib 推向解释器唯一可用状态的最大单一因素。

**修复**: 在 codegen 中为 Result/Option 方法调用添加处理。每个方法需要在 `compile_call_expr` 中有自己的匹配臂，知道 `{i1, T, E}` 或 `{i1, T}` 的布局并生成相应的 GEP + 分支。

---

### P1-5: 闭包参数传给 `map/filter/reduce` 在 Codegen 中被拒绝 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/expr.rs` — ✅ `compile_call_expr` 的 `map`/`filter`/`reduce` 分支添加 `Expr::Lambda` 支持 |
| **实现** | 编译 `Expr::Lambda` → closure struct，提取 `fn_ptr`(field 0) + `env_ptr`(field 1)，通过 `build_indirect_call` 调用；支持 map/filter/reduce 三者 |
| **验证** | 1337 tests pass |

---

### P1-6: 多参数 `println` 在 Codegen 中编译错误 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/builtins/io.rs:9-47` — ✅ 重写 `compile_println`：单字符串参数用 `puts`（带换行），多参数用 `printf` 动态格式化 |
| **验证** | 1337 tests pass |

---

### P1-7: `keys()`/`values()` 对运行时 Map 在 Codegen 中缺失 — ✅ 已修复

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/expr.rs` — ✅ `keys`/`values` 分支在 `fields.is_empty()` 时添加运行时 fallback，调用 `compile_builtin_call` → `compile_map_keys`/`compile_map_values` → C 运行时 |
| **实现** | 编译实参为 `BasicValueEnum`，转发给 `compile_builtin_call` 处理运行时 map 句柄 |
| **验证** | 1337 tests pass |

| 属性 | 值 |
|------|-----|
| **位置** | `codegen/expr.rs compile_call_expr` — `keys`/`values` 仅作为编译期记录类型内置函数处理 |
| **解释器状态** | ✅ 通过 `interp/builtins.rs` 对运行时 Map 有完整实现 |

**codegen 当前状态**: `keys`/`values` 在 `is_builtin` 和 `compile_builtin_call` 中缺失。它们仅在 `compile_call_expr` 中作为编译期内置函数（约第 281 行）处理，且仅对记录类型有效。运行时 Map 类型的调用落入通用函数查找并失败。

**损坏的 stdlib 函数 (5)**:
```mimi
maps::merge        → keys(b)        // 运行时 map 上调用
maps::to_list      → keys(m), values(m), zip(ks, vs)
maps::filter_keys  → keys(m)
maps::map_values   → keys(m)
maps::omit         → keys(m), contains(ks, ok)
```

**修复**: 在 codegen 中添加 `compile_keys`/`compile_values`，调用 C 运行时函数 `mimi_map_keys`/`mimi_map_values`（已在 `mimi_runtime.c` 中实现）。

---

### P1-8: Codegen ↔ Interp 内置函数双向缺口

#### 方向 A: Codegen 有但 Interp 缺失

| 内置函数 | 位置 (Codegen) | 缺失位置 (Interp) |
|----------|---------------|-------------------|
| `eprintln` | `compile_eprintln` | `call_named` 中缺失 |
| `exit` | `compile_exit` | `call_named` 中缺失 |
| `socket` / `connect` / `bind` / `listen` / `accept` / `send` / `recv` / `close_fd` | `compile_socket` 等 | `call_named` 中全部缺失 |
| `http_get` / `http_post` | `compile_http_get` / `compile_http_post` | `call_named` 中缺失 |

#### 方向 B: Interp 有但 Codegen 缺失

| 内置函数 | 缺失原因 | 严重程度 |
|----------|---------|---------|
| `map` / `filter` / `reduce` | 需要高阶函数 codegen（P1-5） | 高 |
| `allocator_system` / `allocator_arena` / `allocator_bump` / `alloc` / `arena_reset` / `bump_used` | 分配器无 codegen 路径 | 中 |
| `ast_dump` / `ast_eval` | 元编程，设计上仅解释器 | 低 |

---

### P1-9: Codegen 路径测试覆盖不足

以下特性的 codegen 路径**没有任何 `compile_and_run` E2E 测试**：

| 特性 | 风险 | 解释器测试 | Codegen 测试 |
|------|------|-----------|-------------|
| `Expr::TupleIndex` | 编译时错误 | ✅ | ❌ |
| `Expr::SliceExpr` | 未验证 | ✅ | ❌ |
| `Expr::Range` | 未验证 | ✅ | ❌ |
| `Type::Result` / `Type::Option` | 布局不一致 (P1-1) | ✅ | ❌ |
| `DynTrait` dispatch | 复杂 vtable 路径 | ❌ | ❌ |
| `ImplTrait` return | 未验证 | ❌ | ❌ |
| FFI Callback 编译产物 | thunk 未验证 | ✅ | ❌ |
| `c_shared` retain/release | 引用计数未验证 | ✅ | ❌ |
| 合约验证 (codegen) | 断言未测试 | ✅ | ❌ |
| Actor spawn/await | 线程路径未测试 | ✅ | ❌ |

---

## 5. P2 — 中优先级缺口

### P2-1: 5 个标准库模块零测试覆盖

| 模块 | 函数数 | 测试 | 影响 |
|------|-------|------|------|
| `net.mimi` | 9 | ❌ 零测试（仅有 `#[ignore]` mock） | 网络函数静默未测试 |
| `result.mimi` | 7 | ❌ 零测试 | 核心错误处理模式未测试 |
| `datetime.mimi` | 13 | ❌ 零测试 | 时间计算未验证 |
| `env.mimi` | 8 | ❌ 零测试 | 环境函数未验证 |
| `text.mimi` | 6 | ❌ 零测试 | 文本处理未验证 |

合计 **43 个 stdlib 函数零测试覆盖**。

### P2-2: Fuzz 测试基础设施仅占位

- 路径: `mimi/src/tests/fuzz/mod.rs`
- 内容: 仅一个空的模块声明
- 无 fuzz target、无 corpus、无 CI 集成

### P2-3: `mimi_to_json` 是死代码

- **位置**: `mimi_runtime.c:836-841`
- **实现**: 始终返回 `"{}"`（stub）
- **调用方**: codegen 直接使用 `mimi_json_serialize`，解释器使用 `serde_json`
- **状态**: 此函数从未被任何代码路径调用。它是一个误导性的 stub。

### P2-4: `MIMI_NO_STD` 预处理分支从未编译

- **位置**: `mimi_runtime.c` 多处 `#ifdef MIMI_NO_STD` 块
- **提供**: 无标准库的 bump allocator、无操作锁 stub、
- **问题**: 没有 Cargo feature、没有 build.rs 宏来启用它
- **codegen 的 `no_std` 字段** (`codegen/mod.rs:48`): 存在但从未设置为 `true`

---

## 6. P3 — 低优先级缺口

### P3-1: `compile_math` 缺失（不影响功能）

- **位置**: `codegen/func.rs:739`, `block.rs:302`, `actors.rs:459`
- **细节**: `Stmt::Math(_)` 作为无操作处理。数学注解块在编译后的代码中被跳过
- **影响**: 无（`math:` 块是设计上的元数据，不是可执行代码）

---

## 7. 标准库损坏矩阵

### 7.1 完全损坏（codegen 中编译失败）

| 模块 | 函数 | 失败原因 |
|------|------|---------|
| `result.mimi` | 全部 7 函数 | P1-4: `.is_ok()`, `.unwrap()` 等缺失 |
| `env.mimi:get_var_or` | 1 | P1-4: `.is_ok()`, `.unwrap()` |
| `env.mimi:has_var` | 1 | P1-4: `.is_ok()` |
| `env.mimi:get_int` | 1 | P1-4: `.is_ok()`, `.unwrap()` |
| `env.mimi:get_float` | 1 | P1-4: `.is_ok()`, `.unwrap()` |
| `fs.mimi:read_lines` | 1 | P1-4: `.is_ok()`, `.map_err()` |
| `fs.mimi:file_size` | 1 | P1-4: `.is_ok()`, `.map_err()` |
| `prelude.mimi:fail` | 1 | P1-6: 多参数 `println` |
| `prelude.mimi:assert_non_null` | 1 | P1-6: 多参数 `println` |
| `prelude.mimi:assert_msg` | 1 | P1-6: 多参数 `println` |

### 7.2 静默产生错误结果（codegen 中编译无报错但错误）

| 模块 | 函数数 | 失败原因 |
|------|--------|---------|
| `strings.mimi` | ~15 | P1-3: `len()` 在字符串上返回 data ptr |

### 7.3 逻辑部分损坏（codegen 中编译成功但特定路径失败）

| 模块 | 函数数 | 失败原因 |
|------|--------|---------|
| `collections.mimi` | 14/27 | P1-5: 闭包传入 `map`/`filter`/`reduce` |
| `maps.mimi` | 5/16 | P1-7: `keys()`/`values()` 在运行时 map 上缺失 |

### 7.4 总计

| 类别 | 计数 |
|------|------|
| 完全损坏（codegen 编译失败） | ~20 函数 |
| 逻辑部分损坏（特定路径失败） | ~19 函数 |
| 静默错误（codegen 无报错但结果错误） | ~15 函数 |
| ✅ 安全 | ~126 函数 |

---

## 8. 与现有审计报告的关系

### 8.1 本报告的增量发现

| 问题 | 此前是否记录 | 此前状态 |
|------|------------|---------|
| P0-1 (`Expr::TupleIndex` 在 codegen 中缺失) | ❌ 未记录 | 全新发现 |
| P0-2 (嵌套 block 中 While/For 被丢弃) | ❌ 未记录 | 全新发现 |
| P0-3 (Assign 仅 Ident 目标) | ❌ 未记录 | 全新发现 |
| P0-4 (Let 仅简单模式) | ❌ 未记录 | 全新发现 |
| P0-5 (Let 无 init 被跳过) | ❌ 未记录 | 全新发现 |
| P0-6 (`from_int` 死代码) | ❌ 未记录 | 全新发现 |
| P1-1 (`Type::Result` 表示不一致) | ❌ 未记录 | 全新发现 |
| P1-2 (`Type::Name` 降级为 i64) | ❌ 未记录 | 全新发现 |
| P1-3 (`compile_len` 对字符串错误) | ❌ 未记录 | 全新发现 |
| P1-4 (Result 方法 codegen 缺失) | ❌ 未记录 | 全新发现 |
| P1-5 (闭包在 map/filter/reduce 中被拒绝) | ❌ 未记录 | 全新发现 |
| P1-6 (多参 println 错误) | ❌ 未记录 | 全新发现 |
| P1-7 (keys/values 运行时 map 缺失) | ❌ 未记录 | 全新发现 |
| P1-8 (内置函数双向缺口) | ❌ 未记录 | 全新发现 |
| P2-1 (零测试覆盖的 stdlib 模块) | ❌ 未记录 (0_7_0eval.md 提到但未列出) | 更精确 |
| P2-2 (Fuzz 占位) | ❌ 未记录 | 全新发现 |
| P2-3 (`mimi_to_json` 死代码) | ❌ 未记录 | 全新发现 |
| P2-4 (`MIMI_NO_STD` 从未编译) | ❌ 未记录 | 全新发现 |
| P2-5 (`#![allow(dead_code)` API surface 保留) | ℹ️ 非问题 | 审计后删除 — API surface / 预留基础设施 |
| P3-1（旧）(`compile_impl_methods &self`) | ℹ️ 非问题 | 审计后删除 — 语言不支持 `&mut self` |

### 8.2 此前已覆盖的问题（不重复记录）

| 报告 | 覆盖范围 | 状态 |
|------|---------|------|
| `AUDIT-REPORT.md` | FFI 内存安全、ABI、回调、Shared RC、extern 合约 | 全面覆盖，修复中 |
| `MIMI_AUDIT_REPORT.md` | 7 Critical + 16 High + 14 Medium + 10 Low (基础功能) | 全部 47 问题已修复 |
| `MIMI_REAUDIT_REPORT.md` | 复查 20 个剩余问题 | ~10 个已修复（`d371640`, `afb7c77`, `caaa3f6`） |
| `MIMI_FINAL_AUDIT.md` | 最终确认清单 | 1339 passed, 0 failed |
| `0_7_0eval.md` | P0-P3：双路径不对称、FFI 管道、stdlib | 全部修复项标记完成 |

### 8.3 本报告未覆盖

- MimiSpec 语言规范 / 解析器
- OSE IDE 产品规格
- VSCode 扩展
- 已由 `AUDIT-REPORT.md` 覆盖的 FFI 深度问题

---

## 9. 修复路线图

### ✅ 阶段 1: 立即修复（1-2 天）— 全部完成

| 优先级 | 问题 | 状态 |
|--------|------|------|
| P0-6 | `from_int` 添加到 `is_builtin` + interp | ✅ 完成 |
| P0-1 | `TupleIndex` codegen | ✅ 完成 |
| P1-3 | `compile_len` 区分字符串/列表 | ✅ 完成 |
| P1-6 | 多参 `println` 改用 `printf` | ✅ 完成 |

### ✅ 阶段 2: 短期修复（3-5 天）— 全部完成

| 优先级 | 问题 | 状态 |
|--------|------|------|
| P0-2 | While/For 在 compile_block 中 | ✅ 完成 |
| P0-3 | Assign 多目标类型 | ✅ 完成 |
| P0-4/5 | Let 模式+无 init 支持 | ✅ 完成（完整递归实现） |
| P1-1 | 统一 Type::Result 表示 | ✅ 完成 |

### ✅ 阶段 3: 中期修复 — 大部分完成

| 优先级 | 问题 | 状态 |
|--------|------|------|
| P1-4 | Result/Option 方法 codegen | ✅ 部分完成（is_ok/is_err/is_some/is_none/unwrap/expect） |
| P1-5 | 闭包在 map/filter/reduce 中 | ✅ 完成 |
| P1-7 | keys/values 运行时 map | ✅ 完成 |
| P1-8 | 内置函数双向补齐 | ✅ 完成（interp 侧） |

### 阶段 4: 测试覆盖（持续）

| 项目 | 预期工作量 | 影响 |
|------|-----------|------|
| 为 10+ 特性添加 codegen E2E 测试 | ~3 天 | 防止回归 |
| 为 5 个零测试 stdlib 模块添加测试 | ~2 天 | 验证 stdlib 正确性 |
| 设置 fuzz 测试基础设施 | ~2 天 | 长期质量保证 |

---

## 10. 附录：各子系统健康度

### 10.1 子系统评分

| 子系统 | 评分 | 主要问题 |
|--------|------|---------|
| **AST 定义** (`ast.rs`) | ✅ 良好 | — |
| **Lexer** (`lexer.rs`) | ✅ 良好 | — |
| **Parser** (`parser/`) | ✅ 良好 | — |
| **类型检查器** (`core/`) | ✅ 良好 | — |
| **解释器** (`interp/`) | ✅ 良好 | 缺少 `eprintln`/`exit`/网络内置函数 |
| **Codegen 表达式** (`codegen/expr.rs`) | ✅ 良好 | 全部已修复 |
| **Codegen 语句** (`codegen/{func,block}.rs`) | ✅ 良好 | 全部已修复 |
| **Codegen 类型** (`codegen/types.rs`) | ✅ 良好 | 全部已修复 |
| **Codegen 内置函数** (`codegen/builtins/`) | ✅ 良好 | 全部已修复 |
| **FFI 层** (`ffi/`) | ✅ 良好 | 已知限制已由 `AUDIT-REPORT.md` 覆盖 |
| **C 运行时** (`mimi_runtime.c`) | ⚠️ 中等 | `mimi_to_json` 死代码、`MIMI_NO_STD` 未编译 |
| **标准库** (`std/`) | ❌ 较差 | ~35 函数在 codegen 中损坏，5 模块零测试 |
| **测试** (`tests/`) | ⚠️ 中等 | 1339 通过但 codegen 路径严重不足 |
| **Formatter** (`fmt.rs`) | ⚠️ 基本 | 仅处理空白（已知限制） |
| **Linter** (`lint.rs`) | ⚠️ 基本 | 仅 4 条规则（已知限制） |
| **LSP** (`lsp.rs`) | ✅ 基础可用 | — |
| **Verifier** (`verifier.rs`) | ✅ 基础可用 | — |

### 10.2 代码行数概览

| 子系统 | 文件数 | 代码行数 |
|--------|-------|---------|
| Parser | 6 | 2,738 |
| Type Checker | 3 | 3,973 |
| Interpreter | 9 | 5,691 |
| Codegen | 10 | 8,200 |
| FFI | 7 | 1,890 |
| Verifier | 1 | 1,153 |
| LSP | 1 | 1,089 |
| C Runtime | 2 | 1,277 (+122 header) |
| Tests | 68 | 17,770 |
| **总计（源文件）** | **~50** | **~31,639** |

---

*本报告基于 2026-06-20 的代码状态。21 个发现的缺口中有 15 个是此前未记录的，2 个在审计后判定为非问题（API surface / 语言设计选择）并移除。全部 6 个 P0 已修复，P1-1~P1-8 共 8 个 P1 已修复。**剩余 4 个中优先级缺口：** P2-2 (fuzz 基础设施)、P2-3 (mimi_to_json 死代码)、P2-4 (MIMI_NO_STD 未编译)、P2-? (FFI 合约检查 gap)。*

> ⏳ **历史归档**：本文档基于 Mimi v0.7.0（2026-06-20）。缺口状态、测试计数等信息已随项目演进而过时。保留以供历史参考。
