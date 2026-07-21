# Invariant-Driven Development (IDD) Guide

> Mimi 仓库的标准开发流程。所有新功能和 Bug 修复必须遵循此流程。

---

## 1. 三层不变量

| 层级 | 名称 | 断言 | 失败含义 |
|------|------|------|---------|
| **L1** | 双后端等价性 | `run_source(p) == compile_and_run(p)` | 代码生成损坏 |
| **L2** | 类型系统健全性 | `check_source(bad_p) → Err` | 类型检查器漏报 |
| **L3** | 内存安全 | Valgrind/Miri/ASan 零警告 | 未定义行为 |

优先级：L1 > L2 > L3。L1 失败时禁止提交新功能。

---

## 2. 新增功能流程

```
1. 创建 feature 分支: git checkout -b feat/xxx
2. 编写 L1 双后端测试（允许暂时 #[ignore]）
3. 在解释器中实现
4. 在代码生成中实现
5. 添加 L2 健全性测试
6. 运行 L3 内存检查（如适用）
7. 提交（引用测试名与不变量类别）
8. 合并回 main
```

---

## 3. 修复 Bug 流程

```
1. 创建 fix 分支: git checkout -b fix/xxx
2. 编写重现该 Bug 的 L1/L2 测试（应失败）
3. 修复代码
4. 测试通过
5. 补充通用回归测试
6. 提交（引用测试名）
7. 合并回 main
```

---

## 4. 已知差距处理

- 暂时失败的测试标记 `#[ignore = "reason"]`
- 每个 `#[ignore]` 必须在下方差距表中登记
- 关闭差距：修复代码 → 解除 `#[ignore]` → 更新文档 → 提交

### 已知 Codegen 差距速查

| 差距 | 解释器 | Codegen | 状态 |
|------|--------|---------|------|
| `from_json::<T>` 类型化反序列化 | ✅ | ✅ | 已实现（i32/i64/f64/bool/string 字段） |
| `Set<T>` 操作 | ✅ | ✅ | 已实现（literal/contains/size/insert/remove/to_list） |
| `sort_f64` / `sort_str` | ✅ | ✅ | 已实现（runtime `mimi_sort_f64_inplace` / `mimi_sort_str_inplace`） |
| `const` 关键字 | ✅ | ✅ | 已实现（标量 + string + 函数调用） |
| `exec(...)` Record 布局 | ✅ | ✅ | 已实现（ExecResult 字段偏移正确） |
| `match` on `Result` in codegen | ✅ | ⚠️ | 部分支持：内层自定义枚举负载的匹配可能失败（见 `e2e_net_fetch_failure`） |
| 递归栈溢出保护 | ✅ | ✅ | 浅递归已支持；极深递归仍依赖宿主栈大小 |
| Comptime 块 | ✅ | ✅ | v0.28.21: codegen `fold_comptime_block` + `fold_comptime_items` 支持 Int/Float/Bool/Unit/String 折叠 |
| Quote!（非纯字面量） | ✅ | ⚠️ | v0.28.21: 三阶段折叠（literal→interp→runtime），运行时依赖报错提示用 `comptime { ... }` 包裹 |
| `ast_eval(ast)` | ✅ | ⚠️ | v0.28.21: 编译期折叠 quote 块直接 pass-through；运行时堆 QuotedAst 暂不支持求值 |
| Valgrind | ✅ | ✅ | 已安装；8 个 Valgrind 测试全部默认通过（含 shared/weak 生命周期 4 个、spawn 多线程 1 个） |
| Miri | ✅ | ⬜ | 解释器子集通过（`tests::basic_*`、`interpreter_features`）；codegen/FFI 测试因 Miri 不支持外部函数/子进程，不纳入 Miri 回归 |
| ASan | ✅ | ✅ | `e2e_asan_*` 已取消 #[ignore]，在可用工具链下通过 |
| 网络 HTTP 失败 | ✅ | ✅ | `e2e_net_fetch_*` 已取消 #[ignore]，连接不可达端口时行为正确 |
| cc-linker fuzz/property | ✅ | ✅ | 已取消 #[ignore]，默认运行并自动跳过 |
| `#[ignore]` 工具链测试 | — | — | 剩余 10 个（见下方登记表） |

### 已知 `#[ignore]` 登记表（v0.31.6）

| 测试 | 原因 | 跟踪 |
|------|------|------|
| `e2e_valgrind_*` (×3) | 需要 Valgrind 已安装 | 工具链门禁 |
| `e2e_asan_*` (×2) | 需要 ASan 工具链 | 工具链门禁 |
| `asan_toolchain_gate` | 需要 ASan/UBSan instrumented pipeline | 工具链门禁 |
| `tricky_record_generic_closure` | 泛型 record 单态化仍为 i64 | v0.31.7+ type engine |
| `tricky_record_generic_pair_field` | 同上 | v0.31.7+ type engine |
| `tricky_push_empty_list_infer` | `push()` 无法向空 list 传播元素类型 | v0.31.7+ inference |
| `verify_unsatisfiable_requires` | `mms{}` 块不再提取合约（AGENTS.md §10 语义） | 设计决策，永久 |

---

## 5. 提交信息规范

```
<type>(<scope>): <简短描述>

<详细描述>

不变量类别: L1 / L2 / L3
测试: <测试名> (<文件路径>)
```

类型：`feat` / `fix` / `test` / `docs` / `refactor` / `chore`

---

## 6. CI 门禁顺序（执行与修复优先级）

```
1.  cargo test                          # 全量测试（当前 4,053 个，0 failed，10 ignored）
2.  cargo test dual_                    # L1 双后端等价性
3.  cargo test "typecheck::"            # L2 类型系统健全性
4.  cargo test ffi_                     # FFI 契约等价性
5.  cargo test codegen_e2e              # 代码生成 E2E
6.  cargo test real_world               # MCDD 真实程序 CLI 回归
7.  cargo test -- --ignored             # 已知差距（必须编译，允许失败）
8.  cargo +nightly miri test ffi::runtime        # L3 Miri UB 检测（FFI runtime 子集）
9.  cargo +nightly miri test basic_control_flow  # L3 Miri UB 检测（解释器子集示例）
10. cargo test e2e_valgrind -- --nocapture        # L3 Valgrind 内存安全
```

规则：
- 第 1 项失败 → 禁止提交新功能，先修复基础设施。
- 第 2 项失败 → 后端语义分歧，必须优先修复。
- 新增功能的 L1 测试不可跳过；与代码一起提交。

注意：
- Valgrind/Miri 测试需要外部工具链；在可用环境中单独运行。
- 当前全量测试通过数：4053，0 failed，10 ignored。
- `cargo test -- --ignored` 允许失败，但所有被忽略测试必须能编译。

---

## 7. 分支开发规范

- `main` 分支保持可发布状态
- 功能开发在 `feat/*` 分支
- Bug 修复在 `fix/*` 分支
- 每个小版本完成后合并回 `main`

---

## 8. 案例映射

| 版本 | IDD 流程 | 结果 |
|------|----------|------|
| v0.28.5 exec | L1 先行 → interp → codegen → 7 测试通过 | ✅ |
| v0.28.5 file_stat | L1 先行 → interp → codegen → 2 测试通过 | ✅ |
| v0.28.5 append_file | L1 先行 → interp → codegen → 1 测试通过 | ✅ |
| v0.28.5 set_env | L1 先行 → interp → codegen → 1 测试通过 | ✅ |
| v0.28.10 sort_str codegen | L1 先行 → runtime helper → codegen 集成 → 4 测试通过 | ✅ |
| v0.28.10 Set/sort/from_json/const 缺口清零 | 5 大差距全部关闭，L1 测试覆盖 | ✅ |
| v0.28.12 package manager | L1 先行（22 测试）→ 增量强化（13 测试）→ registry 协议文档 | ✅ |
| v0.28.13 math builtins | sin/cos/tan/asin/acos/atan/atan2/sinh/cosh/tanh/ln/log/log2/log10/exp/exp2/cbrt — interp+codegen+infer → L1 41 测试 | ✅ |
| v0.28.13 std/array.mimi | array_new/fill/slice/rotate/binary_search/etc — run_with_stdlib 辅助 → L1 24 测试 | ✅ |
| v0.28.13 std/iter.mimi | iter_range/zip/enumerate/take/drop/chain/repeat/count/unique — L1 19 测试 | ✅ |
| v0.28.13 codegen inline/GVN scaffold | small-fn heuristic + CSE cache + pure tracking — 8 测试 | ✅ |
| v0.28.14 诊断与格式化 | 错误恢复继续解析、多位置诊断、formatter 覆盖剩余语法、lint 规则扩展 | ✅ |
| v0.28.15 安全审查与自举准备 | 关闭 `#[ignore]` 差距、unsafe 审计 ~270 条、MiRI/ASan/Valgrind 回归 | ✅ |
| v0.28.16 Codegen 根基补强 | shared/weak 生命周期 4 个 Valgrind 测试、Miri UB 修复、spawn 线程栈泄漏 | ✅ |
| v0.28.17 CLI 一致性 | CLI 类型检查器统一、`use std::xxx` 语义、`mimi init <path>`、SAFETY 注释 59 处 | ✅ |
| v0.28.18 FFI 导出完整 | 复杂 repr(C) struct-by-value 返回、跨线程 callback、FFI 参数布局验证 | ✅ |
| v0.28.19 Actor Codegen 真实并发 | mailbox + worker thread + self-call 死锁避免 + 1000 次压力测试 | ✅ |
| v0.28.20 并发原语 | Mutex&lt;T&gt;、AtomicI32/I64/Bool、Channel&lt;T&gt; — 11 L1 dual 测试 | ✅ |
| v0.28.21 Comptime/Quote Codegen | comptime 块 codegen 折叠 + quote 三阶段折叠 + 13 L1 测试 + usability DX pass | ✅ |
| v0.28.26 质量止血 + JSON 贯通 | from_json/to_json List<RecordType codegen、Any to_string、35 real_world 全绿 | ✅ |
| v0.28.27 真实代码 codegen 冲刺 | reduce lambda、trait self、newtype、泛型 ADT、包导入 | ✅ |
| v0.28.28 Actor 用户函数可调用 | mimichat #1: actor worker 共享 program 上下文 | ✅ |
| v0.28.29 from_json List mutate | from_json::<List<T>> 返回 owned 可变 List | ✅ |
| v0.28.30 Actor 字段原地 mutate | self.field 写回 + codegen map_get string ABI 修复 | ⬜ |

---

*本指南随 Mimi 实现演进同步更新。*
