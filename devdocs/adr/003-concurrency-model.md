# 3. Mimi 并发模型

## 状态

**已接受**（v0.10 实现，v0.15 计划补齐剩余 gap）

## 上下文

Mimi 语言需要为系统编程提供并发能力，同时保持其核心定位：合约验证、结构化并发、零开销 FFI。设计时面临以下约束：

1. **C runtime 目标**：编译后二进制不应依赖 async runtime 或大型运行时库，适合嵌入和跨语言场景。
2. **双后端等价**：解释器和 LLVM codegen 的行为必须一致（这是 IDD L1 不变量）。
3. **结构化保证**：并发原语必须支持静态分析（Z3 验证）和编译期安全检查（W005 写-写竞争检测）。
4. **Actor 生态需求**：提供高级抽象以降低消息传递并发的使用门槛。

## 决策

采用**三层并发模型**：

### 第一层：spawn/await —— 基础并发原语

- `spawn expr` 将表达式求值放入新 OS 线程执行，立即返回 `Future<T>`。
- `await expr` 消费 `Future<T>`，阻塞当前线程直到结果就绪，提取 `T`。
- 编译后端通过 `pthread_create` / `pthread_join` 实现（`src/codegen/scope.rs:13-39`），解释器通过 `std::thread::spawn` + `mpsc::channel` 实现（`src/interp/eval/expr.rs:586-628`）。
- `spawn` 类型推断结果为 `Future<T>`（`src/core/infer_expr.rs:28`），`await` 解包为 `T`（`src/core/infer/helpers.rs:132`）。

### 第二层：parasteps —— 结构化并发块

- `parasteps { ... }` 块内所有 `spawn` 语句并发执行，块结束时自动 join 所有未 await 的线程。
- 提供编译期静态检查：
  - `W005`：检测对同一 `shared` 变量的并发写入冲突（`src/core/check_stmt.rs:29-475`）。
  - `E0305`：禁止在 `parasteps` 中捕获 `local_shared`。
  - `requires` / `ensures` 合约在 `parasteps` 上下文中被识别和提取。
- codegen 路径：`enter_parasteps()` / `leave_parasteps()` 管理线程 ID 列表，离开时对未 join 的线程调用 `pthread_join`。

### 第三层：Actor —— 消息传递抽象

- `type Actor { fields; methods }` 声明一个 actor 类型。
- `Actor.spawn()` 在解释器中创建独立线程 + 邮箱（mailbox），返回 actor 句柄。
- `actor.method(args)` 通过 mailbox 发送消息，异步执行方法。
- `await actor.method(args)` 发送消息并阻塞等待返回值。
- **解释器路径完全实现**（`src/interp/eval/expr.rs:586-628`，`src/interp/value.rs:ActorMailboxMsg`）。
- **codegen 路径仅为 Actor 结构体生成代码**（`src/codegen/actors.rs:67-109`），邮箱/线程部分**未实现**——actor 在编译路径退化为普通结构体，方法为同步调用。

### 网络套接字配置

- TCP socket 默认设置 `TCP_NODELAY`（禁用 Nagle 算法）和 `SO_REUSEADDR`（快速重用 TIME_WAIT 端口），见 `src/interp/builtins/net.rs:22-63`。

### 为什么选择 pthread 而非 async runtime

| 考量 | pthread | async runtime (tokio/async-std) |
|------|---------|-------------------------------|
| 零成本 FFI | ✅ C ABI 直接匹配 | ❌ 需要 runtime 桥接 |
| 二进制大小 | ✅ 无额外依赖 | ❌ runtime 通常 >100KB |
| C runtime 兼容性 | ✅ 与 mimi_runtime.c 一致 | ❌ 需要 Rust runtime 初始化 |
| 并发模型复杂度 | ✅ 1:1 线程，语义清晰 | ❌ 协作式调度增加心智负担 |
| 嵌入场景 | ✅ POSIX 通用原语 | ❌ 受宿主 runtime 约束 |

Pthread 最适合 Mimi 的"生产编译后端"定位：简单、可预测、与 LLVM codegen 自然对齐。

## 后果

### 正面

1. **双后端等价**：解释器和 codegen 都基于 OS 线程，语义一致。
2. **结构化安全**：`parasteps` 提供作用域级别的 join 语义，防止线程泄漏。
3. **静态可验证**：Z3 验证器可以分析 `spawn` / `await` 表达式（v0.13 已补齐 `Expr::Spawn` / `Expr::Await` 编码路径）。
4. **最小依赖**：编译后二进制仅依赖 libc/libpthread，无 Rust async runtime 包袱。
5. **Network 配置合理**：`TCP_NODELAY` + `SO_REUSEADDR` 减少网络编程陷阱。

### 负面

1. **Actor codegen 未完成**：`type Actor { ... }` 在编译路径退化为同步结构体，邮箱/线程机制仅在解释器生效。这意味着 actor 在 `mimi build` 编译后的行为与 `mimi run` 不同（违反 IDD L1 不变量）。
2. **缺少 async/await 语法 codegen**：`async func` / `await` 的 LLVM codegen 计划在 v0.15 实现（当前仅解释器支持）。
3. **1:1 线程模型的开销**：每个 `spawn` 创建一个 OS 线程，不适合细粒度任务（缺乏工作窃取调度器）。
4. **Z3 验证限制**：actor 方法内部的合约在邮箱异步执行的上下文中无法被 Z3 跟踪。

### 缓解措施

- Actor 在 codegen 中的退化行为已记录为已知 gap，计划 v0.15 补齐。
- 对于高性能并发场景，建议使用 `parasteps` + `shared` 而非 actor。
- 线程创建开销可通过线程池优化（已有 `pool_ensure_init` 机制，见 `mimi_runtime.c`）。
