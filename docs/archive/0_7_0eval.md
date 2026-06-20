# Mimi v0.7.0 系统性评估报告

> 评估日期：2026-06-19 | 代码基线：`505decc` | 测试基线：1162 passed, 0 failed

---

## 一、MimiSpec 规范版本凝固与新特性适配

**严重度：中（P2）**

### 认识纠偏

MimiSpec（`.mms`）和 Mimi（`.mimi`）是**两门独立的语言**，不是同一语言的两个视图：

| 维度 | MimiSpec（`.mms`） | Mimi（`.mimi`） |
|------|-------------------|-----------------|
| 定位 | 高信息密度**意图描述**语言 | **系统编程**语言 |
| 语法 | 基于缩进（4 空格倍数） | 基于花括号 `{ }` |
| 使用者 | 人类 + AI 协作（对话式规约） | 编译器（运行时 + 验证） |
| 核心概念 | Fragment、实体、约束、意图后缀 | 类型、函数、trait、所有权、并发 |

两者共享关键字子集（`requires`/`ensures`/`desc`/`rule`/`math`）——这些是两门语言之间的**语义绑定点**，但其含义和上下文不同。

### 真正的问题：新 Mimi 特性是否需要 MimiSpec 表达层？

Mimi 在 v0.7.0 中已实现的新特性，是否需要 MimiSpec 层面的表达方式？

#### 不需要扩展的点（保持分离）

| 特性 | 理由 |
|------|------|
| `async`/`actor` | MimiSpec 的 `flow`（状态机）已能表达 actor 生命周期 |
| `<T>` 泛型、`trait`/`impl` | Mimi 的泛型是编译器运行时概念，不属于意图描述层 |
| `shared`/`weak` 内存模型 | 属于编译器实现细节，不是需要 AI 讨论的意图 |
| `comptime`/`quote!` | 元编程是编译器内部机制，不属于规范对话 |
| `extern "C"` FFI | FFI 是编译器互操作能力，MimiSpec 无需感知 |
| `cap` 能力类型 | MimiSpec 已有 `with Cap` 能力声明（未来预留接口），方向一致 |
| `unsafe` 块 | 编译器安全逃逸，不属意图描述范畴 |

#### 值得考虑扩展的点

| 特性 | 建议 |
|------|------|
| `requires`/`ensures` 结构化表达式 v.s. Z3 SMT | MimiSpec 当前支持比较运算符 + `and`/`or`/`not`/`in`，已足够表达 Mimi 中的合约条件。**但 `rule`（自然语言约束）→ Z3 可验证合约的映射机制需要定义** |
| `match`/枚举模式匹配 | MimiSpec 已有 `type` 枚举（`A \| B`），`match` 是编程语言的控制流。MimiSpec 的 `if`/`else` 已覆盖条件分支，无需 `match` |
| `math:` 块精度与 Mimi 编译 | `math:` 块与 Mimi 编译器之间目前无连接。若需将 MimiSpec 的数学推导编译为 Mimi 代码，需定义映射关系 |

### 关键缺口

1. **`rule` → `requires`/`ensures` 映射未定义**：MimiSpec 中 `rule "支付必须幂等"` 是自然语言约束，Mimi 编译器的 Z3 验证器无法理解。若要让 `.mms` 中的约束自动产生 `.mimi` 的 `requires`/`ensures`，需要定义映射机制
2. **`.mms` 与 `.mimi` 之间的双向映射**：当前两个语言之间无结构化桥梁。例如无法将 `.mms` 的 `flow` 状态机自动导出为 `.mimi` 的 enum + match

### 建议

1. 明确 MimiSpec 和 Mimi 是两门独立但通过 `requires`/`ensures`/`desc`/`rule` 语义绑定的语言，在 AGENTS.md 及项目 README 中记录
2. 将`是否扩展 MimiSpec 覆盖新特性`作为 v1.0 的设计决策，而非已承诺的功能
3. ✅ **P2**：定义 `rule`（自然语言约束）→ `requires`/`ensures`（结构化表达式）+ Z3 断言之间的映射机制

---

## 二、解释器与 Codegen 双路径测试不对称

**严重度：极高（P0）**

### 覆盖矩阵

| 特性 | Interp 测试 | Codegen 测试 | Interp 实现 | Codegen 实现 | 风险 |
|------|:---:|:---:|:---:|:---:|:----:|
| `dyn Trait` vtable 分发 | ❌ | ❌ | ❌（仅 display） | ✅（完整 fat pointer + vtable） | **严重** |
| `from_json` / `to_json` | ❌ | ❌ | ✅（serde_json） | ✅（C 递归下降解析器） | **严重** |
| Actor / async | ✅ 完整 | ⚠️ 仅 IR 验证 | ✅ | ✅ | 中 |
| FFI / extern | ✅ 完整 | ⚠️ 仅 IR 验证 | ✅ | ✅ | 中 |
| `impl Trait` opaque | ⚠️ 2 个基础测试 | ❌ | ❌（仅 display） | ✅ | 高 |
| comptime / quote | ✅ 完整 | ⚠️ 仅 error-path | ✅ | ❌ 故意拒绝 | 低（设计如此） |

### 关键缺口

- **`dyn Trait` 最大缺口**：codegen 有完整实现（`codegen/expr.rs:643-744`），但零测试。解释器完全不支持。
- **JSON 全实现但零测试**：C runtime 含完整递归下降解析器（`mimi_runtime.c:768-941`），但没有任何 Mimi 测试调用过 `from_json`。
- **Actor/FFI 从未以 E2E 方式执行**：codegen 测试只验证 IR 结构正确，从未 `compile_and_run`。

### 建议

1. **P0**：为 `dyn Trait` 添加 interp 实现 + 双路径测试
2. **P0**：为 `from_json` 建立 JSON 规范边界测试套件
3. **P1**：为 Actor async 编写 E2E `compile_and_run` 测试

---

## 三、FFI 验证管道中间断裂

**严重度：高（P1）**

### 管道现状

```
解析器: extern { requires/ensures }  →  ✅ 解析正确（AST 存储 Expr）
    ↓
AST → FfiContract 映射                →  ✅ 完整（contract.rs:124-129）
    ↓
解释器: --verify-ffi 运行时检查       →  ✅ 通过 eval_expr() 实现
    ↓
Z3 形式验证                            →  ❌ 完全不处理 extern 块
    ↓
Codegen: extern wrapper 合约断言       →  ❌ wrapper 不插入 requires/ensures
    ↓
#[repr(C)] enum ABI 兼容性              →  ❌ LLVM IR 发 {i32, i64}，C 头文件生成裸 int
```

### 关键断裂点

1. `mimi verify` 命令**完全不处理 `Item::ExternBlock`**
2. Codegen extern wrapper **不插入 `compile_contract_assert()`**
3. `#[repr(C)]` enum 的 LLVM IR 布局（`{i32,i64}` 结构体）与 C ABI（裸 `int`）不匹配
4. `scripts/test-ffi-contracts.sh` 使用注释语法而非 `requires` 关键字，测试内容为空
5. **没有端到端测试**涉及 `extern { requires <expr> }` + 实际评估

### 建议

1. **P1**：编写 E2E 测试——声明带 `ensures: result > 0` 的 extern 函数，引入故意的 C 实现错误，确认运行时捕获
2. **P1**：修复 `#[repr(C)]` enum 的 LLVM IR 布局以匹配 C ABI
3. **P1**：在 codegen extern wrapper 中插入 requires/ensures 断言
4. ✅ **P2**：将 extern 合约送入 Z3 形式验证管道

---

## 四、内存安全 CI 与语言安全语义错位

**严重度：中-高（P1）**

### 现状

| 检测手段 | 状态 | 覆盖范围 |
|---------|------|---------|
| Miri (Rust UB) | ✅ CI 中存在 | 仅解释器路径，跳过 codegen/FFI/e2e |
| ASan (地址消毒) | ✅ CI 中存在 | 仅 Rust 端，跳过 codegen/e2e |
| Valgrind | ❌ 完全缺失 | 从未对 Mimi 编译产物运行 |
| MSan/TSan/UBSan | ❌ 完全缺失 | 未实现 |

### Unsafe 块分布

总计 **74 个 unsafe 块**，全部有 `// Safety:` 注释：

| 类别 | 数量 | 风险 |
|------|------|------|
| LLVM `build_gep` 调用 | **46** | LLVM 端指针类型假设 |
| FFI 运行时 C ABI | 9 | `Box::from_raw`、`CStr::from_ptr` |
| FFI 调用路径 | 4 | `libloading`、`libc` |
| Interp 值管理 | 1 | `libc::free` |
| `SendRc`/`SendWeak` unsafe impl | 2 | 依赖类型检查器保证 |

### 关键缺口

- **Mimi 编译产物的内存安全从未被检测**。`shared`/`weak` 引用计数正确性只在解释器中有 17 个测试
- 没有 valgrind 包装器可用 `compile_and_run()` 测试工具

### 建议

1. **P1**：为 `compile_and_run()` 添加 valgrind 可选包装器
2. **P1**：在 CI 中添加 codegen+e2e 路径的 ASan 覆盖（`cc -fsanitize=address` 链接）
3. ✅ **P2**：编写 Mimi 程序刻意制造 shared 循环引用/weak upgrade 失败场景，通过 valgrind 检测

---

## 五、标准库 17→91 测试覆盖密度

**严重度：高（P0）**

### 模块覆盖概览

16 个模块，282 个公开函数 + 13 个常量。

| 覆盖等级 | 模块 | 占比 |
|----------|------|------|
| ✅ 良好（50-70%） | strings, maps, prelude（核心） | ~35% |
| ⚠️ 部分 | io, fs, result, mymath, collections | ~30% |
| ❌ 接近零/零 | **json**、**net**、datetime、text、env、testing、random | ~35% |

### 具体缺口

#### json 模块：零测试
- `from_json` 在 interp 端通过 `serde_json` 实现，codegen 端通过 C 递归下降解析器实现（`mimi_runtime.c:768-941`），但**没有任何测试**
- `to_json` **codegen 端是 stub**：C runtime 的 `mimi_to_json()` 始终返回 `"{}"`
- `json::is_valid_json` **有 bug**：`from_json('""')` 返回 `""`，`result != ""` 为 `false`——有效的空字符串 JSON 被误判为无效

#### net 模块：零测试
- `tcp_connect`/`fetch`/`fetch_post` 有完整 POSIX 套接字实现（`mimi_runtime.c:1014-1095`），但无 mock，无测试

#### 错误处理不一致
- `fs::read` 等 stdlib wrapper 丢弃底层 `Result` 信息，静默返回 `""`
- 网络函数返回二进制错误字符串（`"connection failed"`），无法程序化区分错误类型
- `json::get_bool` 无法区分 `false` 和 key 不存在（均返回 `false`）

### 建议

1. **P0**：为 `from_json` 建立测试套件，覆盖 JSON 规范所有边界情况（嵌套、转义、Unicode、超大数字、空输入）
2. **P1**：修复 `to_json` codegen 端的 stub
3. **P1**：为网络函数建立 mock 测试（loopback socket）
4. **P1**：审查所有 stdlib 函数错误处理，统一为 `Result<T, string>` 风格
5. **P1**：修复 `json::is_valid_json` 的空字符串 bug

---

## 六、Comptime + Z3 验证交叉未定义行为

**严重度：中（P2）**

### 架构

```
Z3 验证器 (mimi verify)      ← 操作静态 AST，完全独立
                                ↓
解释器 (mimi run)             ← 运行时 eval_expr，支持 verify_contracts
                                ↓
Comptime/quote                ← 仅在解释器中求值，codegen 拒绝
```

### 交互边界

- `comptime { }` 块内调用的函数会经历运行时合约检查（`verify_contracts` 守卫下），**但不会经过 Z3**
- `quote!` 生成的 AST **明确排除** `Requires`/`Ensures` 语句（`quote.rs:40`）
- 生成的代码通过 `eval_quoted_ast()` 执行，**不经任何合约检查**
- 两者之间没有任何组合测试或文档

### 建议

1. ✅ **P2**：明确 comptime 和 Z3 的交互语义
2. ✅ **P2**：编写测试：comptime 中生成违反 ensures 的函数，确认编译器能报错
3. ✅ **P2**：在 AGENTS.mimi.md 中记录此限制

---

## 七、修复清单

### P0（v1.0 阻塞项）

- [x] **P0-1**：为 `dyn Trait` 添加解释器实现 + 双路径测试
  - `interp/eval.rs`：添加 `DynTrait` 方法分发的 dispatch 逻辑
  - `tests/codegen_e2e.rs`：添加 `trait Animal { speak() }` + `dyn Animal` 的 E2E 测试
- [x] **P0-2**：为 `from_json` 建立测试套件
  - 覆盖：嵌套对象、数组、转义字符、Unicode、超大数字、空输入、非法 JSON
- [x] **P0-3**：修复 `#[repr(C)]` enum 的 LLVM IR 布局以匹配 C ABI
  - 当前：`{i32, i64}` 结构体 → 修正：裸 `i32`（C enum）

### P1（高优先级）

- [x] **P1-1**：为 Actor async 编写 E2E `compile_and_run` 测试（`97eaf92`）
- [x] **P1-2**：编写 extern + ensures 的 E2E 测试（确认运行时断言能触发）（`97eaf92`）
- [x] **P1-3**：在 codegen extern wrapper 中插入 `compile_contract_assert()`（`97eaf92`）
- [x] **P1-4**：为 `compile_and_run()` 添加 valgrind 包装器，并在 CI 中启用（`E2EConfig` + `compile_and_run_valgrind()` + GH Actions workflow）（`0987ecf` + `c86a1b8` 加互斥检查）
- [x] **P1-5**：在 CI 中添加 codegen+e2e 的 ASan 覆盖（`cc -fsanitize=address`）（`E2EConfig` + `compile_and_run_asan()` + GH Actions workflow）（`0987ecf`）
- [x] **P1-6**：修复 `to_json` codegen 端的 stub 实现（`97eaf92` int/float/bool/string 内联 + `c86a1b8` 修复 bool 因 i1⊂IntValue 不可达、移除 build_string_result 返回 raw buf、List/Record 降级 stub）
- [x] **P1-7**：修复 `json::is_valid_json` 的空字符串 bug（`97eaf92` C runtime + codegen + 测试 + `c86a1b8` json_is_valid 加入 is_builtin() + 6 个 E2E JSON 测试）
- [x] **P1-8**：审查 stdlib 错误处理一致性（`2026e64` Result 返回 + `c86a1b8` read_lines/file_size 通过 map_err 保留原始 I/O 错误）

### P2（中优先级）

- [x] **P2-1**：将 extern 合约送入 Z3 形式验证管道
  - `verifier.rs`：添加 `verify_extern_func()`，验证 requires 可满足性 + ensures 在给定 requires 下的可行性
  - `verifier.rs`：`verify_items()` 现在处理 `Item::ExternBlock`，为每个带合约的 extern func 生成验证结果
  - `parser/mod.rs`：修复 extern 块中 `requires`/`ensures` 用新行分隔时 post-expr 未 skip_newlines 的 bug
  - 5 个单元测试：consistency、unsatisfiable requires、both requires+ensures、无合约跳过、与普通 func 混合
  - `test-ffi-contracts.sh`：改注释语法为关键字语法，拆分 Z3 静态验证（Phase A）和 `--verify-ffi` 运行时检测（Phase B）
- [x] **P2-2**：编写 shared 循环引用/weak upgrade 失败的 valgrind 测试
  - `ownership.rs`：增强 `weak_upgrade_none_after_drop` 验证返回值为 0（None），新增 `weak_upgrade_none_after_drop_local` 覆盖 LocalShared 路径
  - `ownership.rs`：新增 `shared_cyclic_reference_interp` 测试两 local_shared 共存场景
  - `codegen_e2e.rs`：新增 `e2e_valgrind_shared_weak_lifecycle` 占位测试（标记 `#[ignore]`），待 codegen 实现 Arc/Rc 引用计数后启用
  - 注：当前 codegen 将 `SharedLet` 编译为普通 `let`，无 Arc/Rc 基础设施，完整 cycle leak 测试需等 codegen 升级
- [x] **P2-3**：明确 comptime + Z3 交互语义并写入文档
  - `AGENTS.mimi.md`：新增 §9 "comptime 元编程与 Z3 验证的交互语义"，覆盖架构图、三条独立路径、交互边界、关键限制（quote 排除合约、Z3 不验证 comptime、eval_quoted_ast 绕过 verify_contracts）
- [x] **P2-4**：编写测试：comptime 中生成违反 ensures 的函数，确认报错
  - `comptime.rs`：3 个测试覆盖 comptime 函数合约行为：违反 ensures 被运行时捕获、满足 ensures 正常返回、quote 生成的闭包不含合约
- [x] **P2-5**：为网络函数建立 mock 测试
  - `codegen_e2e.rs`：5 个 `#[ignore]` E2E 测试覆盖 socket 创建、connect 失败、listen/bind、fetch 失败、fetch_post 失败
  - 测试内联 net.mimi 包装逻辑（compile_and_run 不支持 `use` 导入），通过 loopback 连接失败模拟 mock
- [x] **P2-6**：定义 `rule` → `requires`/`ensures`（自然语言 → 结构化合约）的映射机制
  - `AGENTS.mimi.md`：新增 §10 定义四种映射规则（直接表达式、冒号分隔、前缀识别、无法映射时保留为 Desc），明确实现位置和 v1.0 时间线

### P3（低优先级）

- [x] **P3-1**：添加 UBSan CI 作业（MSan/TSan 需完整二进制插桩，不切实际；已添加 `use_ubsan` E2EConfig + 3 UBSan E2E 测试 + `.github/workflows/ci.yml` ubsan job）
- [x] **P3-2**：`json::get_bool` 的 false/missing 区分（返回类型改为 `Result<bool, string>`；`json_get_string` 已验证 `false`/`true`/`null`/数字值的字符串表示）
- [x] **P3-3**：网络函数返回结构化错误码而非字符串（新增 `NetError` 枚举，所有网络包装函数返回 `Result<T, NetError>`，E2E 测试同步更新）

---

> ⏳ **历史归档**：本文档基于 Mimi v0.7.0 代码基线 `505decc`（2026-06-19）。测试计数、版本号等信息已随项目演进而过时。保留以供历史参考。
