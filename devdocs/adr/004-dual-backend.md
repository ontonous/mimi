# ADR-004: 双后端架构（Interpreter + LLVM Codegen）

## 状态

已采纳（v0.7+，持续有效）

## 上下文

Mimi 是 MimiSpec 意图描述的生产编译后端。作为编译型语言，需要同时满足两个矛盾的需求：

1. **快速开发迭代**：在语言设计早期，每次改动都能即时运行和调试
2. **生产级性能**：最终编译产物应接近原生性能，支持系统编程和 FFI

单一实现无法同时满足二者。纯解释器开发周期快但性能差；纯编译器（LLVM codegen）构建缓慢且调试困难。此外，需要一种机制保证两个实现路径的语义一致性。

## 决策

采用**双后端架构**：同时维护一个 tree-walking 解释器和 LLVM IR codegen，二者共享同一 AST 和类型检查器。

### 组件职责

| 组件 | 路径 | 职责 | 使用场景 |
|------|------|------|---------|
| **Interpreter** | `src/interp/` | tree-walking 求值器，直接执行 AST | `mimi run`、`mimi lsp`、单元测试、comptime 元编程 |
| **Codegen** | `src/codegen/` | 通过 inkwell 生成 LLVM IR，链接 C runtime | `mimi build`、`mimi build --verify-contracts` |
| **类型检查器** | `src/core/` | 统一类型推断 + 合约验证 | 两后端共享 |
| **C Runtime** | `src/runtime/mimi_runtime.c` | 引用计数、JSON、线程池、HTTP、正则、time | 仅 codegen 输出链接 |

### 解释器（Interpreter）

- 直接对 AST 进行 `eval_expr` / `eval_stmt` 递归求值
- 值表示：`Value` 枚举（Int, Float, Bool, String, Tuple, List, Func, Shared, Struct 等）
- 支持全部语言特性，包括 `comptime`、`quote!`、actor spawn、FFI
- 合约验证：`call_func()` 路径的 `verify_contracts` 守卫
- 启动快（毫秒级），适合 LSP 和测试

### 代码生成（Codegen）

- 通过 inkwell crate 生成 LLVM IR
- 函数编译到 LLVM 函数，变量映射到 `AllocaInst`
- 共享/引用计数调用 `mimi_rc_alloc/retain/release`
- 字符串通过 `mimi_string_t` 结构体 + C runtime 管理
- 输出 ELF 二进制，链接 `mimi_runtime.o`

### IDD（Invariant-Driven Development）

双后端架构的核心方法论是**不变量驱动开发**：

1. **L1 不变量**：每项功能在两个后端产生相同的输出
2. **L2 不变量**：类型检查器一致地拒绝非法程序
3. **L3 不变量**：Valgrind/Miri/ASan 下零警告

所有新功能必须按此流程：编写 L1 测试 → 解释器实现 → codegen 实现 → 补充 L2 测试 → 内存安全检视 → 提交。

### 已知差距

| 特性 | 解释器 | Codegen | 原因 |
|------|--------|---------|------|
| struct-by-value FFI | ✅ | ❌ | LLVM ABI struct 传递不匹配（X86\_64 ABI 规则复杂） |
| actor spawn/await | ✅ | ❌ | spawn 涉及运行时线程池，codegen 路径未实现 |
| HTTP server（net 模块） | ✅ | ❌ | tcp_accept/recv/send codegen 路径未补齐 |
| async/await 结构化并发 | ✅ | ❌ | interpreter-only，codegen 未实现协程升降级 |

差距使用 `dual_assert_interp_only!` 宏显式标记，并在 `AGENTS.md` §8 和 `docs/idd-guide.md` 中登记。

### 测试策略

```rust
// dual_assert! — 同时在两个后端运行，断言 stdout 输出一致
dual_assert!("func main() -> i32 { println(2 + 3); 0 }", "5");

// dual_assert_interp_only! — 仅运行解释器，用于 codegen 未实现的特性
dual_assert_interp_only!("func main() -> i32 { spawn fn() { 1 }; 0 }", ...);

// dual_assert_contract_ok! — 验证合约验证器接受合法合约
dual_assert_contract_ok!("func abs(x: i32) -> i32 { ... }");
```

177+ L1 双后端等价测试位于 `src/tests/dual_backend.rs`，CI 门禁中作为优先级 2 执行（仅次于全量测试）。

## 后果

### 正面

- **语义一致性**：两项独立实现产生相同结果，极大降低编译器 bug 概率
- **解释器作为参考实现**：快速原型新功能，再移植到 codegen
- **高测试覆盖**：每个 `dual_assert!` 测试同时覆盖两个后端
- **LLVM 升级缓冲**：codegen 因 LLVM API 变更断裂时，解释器保持可用
- **comptime 元编程可行**：解释器在编译期执行 `comptime func`，生成 AST 供后续处理

### 负面

- **开发速度放缓**：每项功能需要实现两次（解释器 + codegen）
- **代码量翻倍**：两个后端各 ~2,000 行，维护成本高
- **LLVM 构建缓慢**：首次构建需编译 LLVM（约 20 分钟）
- **间隙管理负担**：已知差距需持续跟踪，CI 中 `#[ignore]` 测试需要定期检视
- **行为分歧风险**：相同 AST 在不同后端可能产生微妙不同的语义（如整型溢出、浮点舍入）
