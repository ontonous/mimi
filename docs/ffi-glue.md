# Mimi 多语言胶水特性文档

> **版本**：v0.4.0  
> **最后更新**：2026-06-16  
> **状态**：草案，持续演进  
> **实现状态**：基于 Mimi v0.1.1（441 测试通过）

---

## 1. 设计理念

Mimi 在设计之初就将**安全、可审计的跨语言集成**作为一等目标。它不是要取代 C、Rust、TypeScript 或 Python，而是要做那个**主动伸出手、带着权限与契约去安全调用它们**的“信任中枢”。

与 Python 这类传统胶水语言相比，Mimi 的胶水设计解决三个核心痛点：

- **权限隐式化**：传统 FFI 中，任何代码都能调用 `fopen` 或 `socket`。Mimi 要求调用敏感函数必须显式持有并传递**线性能力令牌 (`cap`)**，编译器在编译期就能拒绝未授权访问。
- **意图丢失**：跨过语言边界后，函数的契约、约束、设计意图全部消失。Mimi 会将 `desc`、`rule`、`requires`、`ensures` 等元数据编译进产物，供运行时或 AI 工具消费，让意图可跨语言追溯。
- **补偿手工化**：多语言协作中的事务性错误处理（撤销操作、释放资源）往往需要手工编写 `try/finally` 逻辑，极易遗漏。Mimi 通过内置的 `on failure` 块和 `parasteps`，将 Saga 补偿模式提升为语言构造，由编译器自动编排。

Mimi 的胶水能力是**双向**的：一方面 Mimi 可以安全调用外部语言，另一方面 Mimi 自身也可以被编译为高性能、类型安全的库，供 C、Python、Rust、Swift/Kotlin、TypeScript 等语言直接使用——并在此过程中继续保持权限追踪与意图传递。

**一句话定位**：Mimi 是“**带着安全护栏与意图标签的双向胶水**”——它像 Python 一样便捷地集成现有生态，也能作为可靠的底层库被其他语言安全消费，尤其适合 AI 辅助生成或需要高可信度的集成场景。

---

## 2. 核心架构：基于 `cap` 的能力型 FFI

Mimi 的 FFI 不是简单的“声明外部函数然后调用”。它构建了一套**能力令牌 (Capability Token)** 系统，将外部世界的敏感操作纳入编译器的静态权限检查范围。

### 2.1 能力类型 (`cap`)

`cap` 是一种**线性类型**，代表执行一类外部操作的权利。其性质：

- **不可复制**：不能通过赋值创建多个副本，权限始终唯一。
- **不可隐式丢弃**：必须被显式消费——传递给消耗它的函数，或使用 `drop` 显式释放。编译器会拒绝“忘记处理权限”的代码。
- **必须传递**：被声明为需要某个 `cap` 的外部函数，只能在调用者确实持有该 `cap` 时才能调用。

这意味着：如果你没有 `FileWriteCap`，你连 C 的 `fwrite` 都摸不到。

### 2.2 FFI 声明模式

外部函数通过 `extern` 块声明，**权限依赖直接写在签名里**：

```mimi
cap FileWriteCap;

extern "C" {
    // 需要 FileWriteCap 才能写入文件
    fn write_to_file(path: string, data: string, cap: FileWriteCap) -> Result<(), IOErr>;
}
```

这里 `cap` 参数起到**权限凭证**的作用：调用时传入 `cap`，函数内部可消费它（拿走所有权），也可仅借用。

### 2.3 权限传递与消费

Mimi 对 `cap` 的传递提供精细控制：

- **所有权移动**：函数以 `cap: FileWriteCap` 形式接收，调用后 `cap` 被消费，调用方不再持有。
- **显式释放**：若某个 `cap` 不再需要，必须执行 `drop(cap)` 明确丢弃。

```mimi
func write_twice(path: string, cap: FileWriteCap) -> Result<(), IOErr> {
    // 第一次写入
    write_to_file(path, "first line", cap)?;
    // 第二次写入需要新的 cap（前一个已被消费）
    Ok(())
}
```

编译器在编译期追踪 `cap` 的状态，确保不发生使用已消费权限、重复消费等错误。

### 2.4 `unsafe` 边界与信任降级（规划中）

> ⚠️ **注意**：`unsafe` 块尚未实现，以下为设计目标。

对于确实需要绕过能力检查的底层操作（如实现新的 `cap` 原语），Mimi 计划提供 `unsafe` 块。`unsafe` 内可以调用未声明 `cap` 的外部函数，但：

- 代码必须在编译报告中被标记为 `unsafe`；
- 在 `--strict` 模式下，这些块是 AI 和人类审查的重点；
- `unsafe` 不应出现在常规胶水代码中，只用于封装基础能力。

当前版本中，所有外部函数调用都必须通过 `cap` 授权，没有绕过机制。

---

## 3. 双向胶水支持：Mimi 调用世界，世界调用 Mimi

Mimi 的胶水设计兼顾“向外调用”与“对外暴露”两个方向，形成完整的双向集成能力。

### 3.1 第一梯队：C ABI 直达（向外调用）

任何能导出 C ABI 的语言，Mimi 都能直接调用：

- **C**：最直接的调用，通过标准 C 调用约定。
- **C++**：通过 `extern "C"` 封装后调用。
- **Rust**：通过 `extern "C"` 或 `cbindgen` 导出 C ABI 后调用。
- **Zig、Nim、Go** 等所有具备 C FFI 能力的语言。

### 3.2 第二梯队：WASM 互操作

将 Mimi 编译为 WebAssembly，在浏览器或边缘计算环境中充当“安全胶水”：

- 调用 JS/TS 提供的宿主 API 时，同样需要通过 `cap` 授权（如 `DomCap`, `FetchCap`）。
- 自动生成 TypeScript 类型声明文件 (`.d.ts`)，包含函数签名和从 `desc`/`rule` 提取的文档，让前端开发者安全消费。

### 3.3 第三梯队：Python 原生扩展

Mimi 可以编译为 Python C 扩展模块（`.pyd`/`.so`），直接 `import` 到 Python 中使用：

- 调用 Mimi 函数就像调用普通 Python 函数一样。
- Mimi 编译时生成一份额外的元数据文件（如 `.mimi.meta.json`），包含每个函数的 `desc`、`rule`、`requires`、`ensures`，供 Python 侧的工具或 AI 读取，实现“意图传递”。

### 3.4 第四梯队：移动端 Swift / Kotlin

Mimi 可编译为 C 动态库，并通过各平台的 FFI 机制集成到移动应用中：

- **Swift (iOS/macOS)**：通过模块映射 (module map) 将 Mimi 编译产物封装为 Swift 可用的 API。Swift 调用时，Mimi 函数的 `cap` 由 Swift 端通过初始化令牌获取，确保权限被明确授予。
- **Kotlin (Android)**：通过 JNI 或 Kotlin/Native 的 `cinterop` 工具直接绑定 C 动态库。Mimi 库的权限令牌被建模为 Kotlin 的单例对象，在应用启动时初始化并传递给 Mimi 函数。

在这两种场景中，Mimi 代码都作为底层的**高可靠性业务逻辑或安全策略执行单元**，移动端 UI 层只需调用其暴露的接口，无需关心内部并发、补偿等复杂逻辑。

### 3.5 反向胶水：Mimi 作为库（核心能力）

以上所有场景都依赖一个共同基础：**Mimi 可以被编译为标准 C 动态库**，并附带以下产物：

- **C 头文件**：声明所有公开函数，包含 `cap` 类型的不透明句柄，调用方必须传递权限令牌才能执行操作。
- **元数据文件**：以 JSON 格式描述每个函数的意图、规则、契约，供上层语言的工具链、AI 助手或文档生成器使用。
- **绑定生成器**：自动生成 Rust、Python、Swift/Kotlin、TypeScript 的 idiomatic 封装，让调用者无需关心底层 C ABI。

这意味着 Mimi 不仅是胶水的“使用者”，也是胶水的“提供者”。一个用 Mimi 编写的、带有 `on failure` 和 `parasteps` 的安全支付模块，可以编译成库，被 Rust 后端、Python 脚本、Node.js 服务、iOS 应用等同时调用，且权限控制与意图信息贯穿始终。

---

## 4. 使用示例

### 4.1 安全调用 C 库（SQLite）

```mimi
cap SQLiteCap;

extern "C" {
    fn sqlite3_open(path: string, cap: SQLiteCap) -> Result<SqliteDB, SqliteErr>;
    fn sqlite3_exec(db: SqliteDB, query: string, cap: SQLiteCap) -> Result<(), SqliteErr>;
}

func init_db(path: string, cap: SQLiteCap) -> Result<SqliteDB, SqliteErr> {
    let db = sqlite3_open(path, cap)?;
    Ok(db)
}
```

### 4.2 并发调度多个异构服务

```mimi
func sync_user_data(user_id: u64, fs_cap: &FileSysCap, net_cap: &NetworkCap) -> Result<(), Err> {
    let (local_data, remote_data) = parasteps "加载用户数据" {
        let p = spawn load_from_disk(user_id, fs_cap);
        let q = spawn fetch_from_api(user_id, net_cap);
        await (p, q)
    };

    store_combined(user_id, local_data, remote_data, fs_cap)
}
```

### 4.3 调用 Python AI 生态（受控桥接）

```mimi
cap PythonCap;

extern "C" {
    fn py_import(module: string, cap: PythonCap) -> Result<PyModule, PyErr>;
    fn py_call(mod: PyModule, func: string, args: PyValue, cap: PythonCap) -> Result<PyValue, PyErr>;
}

func ai_classify(text: string, py_cap: PythonCap) -> Result<string, Err> {
    let mod = py_import("transformers", py_cap)?;
    let result = py_call(mod, "classify", PyValue::String(text), py_cap)?;
    Ok(result.to_string())
}
```

### 4.4 Mimi 作为库：被 Swift 调用

Mimi 源码编译为 `libpayment.dylib`，并附带 `payment.h` 和元数据。Swift 端通过模块映射调用：

```swift
// Swift 端
import MimiPayment

let paymentCap = try PaymentCap.initialize()  // 获取能力令牌
let result = try Payment.process(order: myOrder, cap: paymentCap)
```

在整个调用链中，`PaymentCap` 令牌由 Swift 层明确创建并传入，Mimi 内部的所有 `on failure` 补偿和 `parasteps` 并发逻辑对 Swift 透明，但安全性由 Mimi 编译器保证。

---

## 5. 胶水特性对比分析

| 特性 | Python | Lua | Nim | Rust | Swift/Kotlin | **Mimi** |
|------|--------|-----|-----|------|--------------|-----------|
| **FFI 安全性** | 无静态检查 | 无 | 部分 | 高（需 `unsafe`） | 中 | **极高（cap 静态权限）** |
| **意图可传递性** | 无 | 无 | 无 | 无 | 无 | **元数据导出 (desc/rule)** |
| **并发模型** | GIL / asyncio | 单线程 | 线程/异步 | 线程/async | async/actor | **parasteps + actor** |
| **事务补偿** | 手工 try/finally | 手工 | 手工 | 手工 | 手工 | **语言内置 on failure** |
| **AI 可审计性** | 弱 | 弱 | 中 | 中 | 弱 | **强（cap / --strict）** |
| **作为库被调用能力** | 通过 C API | 通过 C API | 通过 C API | 通过 C API | 原生 | **原生 C 库 + 绑定生成** |
| **学习曲线** | 低 | 低 | 中 | 高 | 中 | **中（需理解 cap / 补偿）** |

Mimi 在跨语言集成的安全性和意图传递两个维度上，提供了现有语言所不具备的系统级支持，同时通过编译为 C 库的能力，使其可以融入任何主流生态。

---

## 6. 当前局限与路线图

### 6.1 已实现 (v0.1.1 → v0.7.0)
- `cap` 线性类型及基本消费追踪
- `extern "C"` 声明与解析
- 解释器中概念验证性 FFI 调用（`libloading` 集成）
- `on failure` 补偿块（scope-aware LIFO 执行）
- `parasteps` 并行执行（thread-based parallelism）
- 基础类型检查与借用追踪
- **v0.7.0 新增**：
  - ✅ Actor codegen：结构体类型 + 构造函数生成
  - ✅ Parasteps codegen：顺序执行回退
  - ✅ Spawn/Await codegen：顺序执行回退
  - ✅ Cap type 解析：`Type::Cap(name)` 映射为 i64 不透明句柄
  - ✅ 语句处理：Drop、SharedLet、OnFailure、Arena、Alloc、Desc、Requires、Ensures、Math
  - ✅ MmsBlock 处理：在 codegen 中跳过（仅文档/契约）
  - ✅ lexer()/parse() 内置函数：使用 mimispec crate 进行词法分析和解析
  - ✅ Comptime 函数执行验证

### 6.2 短期 (v0.2.0)
- 完善 `cap` 的借用与所有权语义
- FFI 类型转换自动化（Mimi ↔ C 类型映射）
- 提供 SQLite、libcurl、TensorFlow Lite 三个关键 C 库的 Mimi 封装示例

### 6.3 中期 (v0.5.0)
- `unsafe` 块支持（绕过 cap 检查的受控机制）
- 自动 FFI 绑定生成器，从 `.h` 文件生成 `cap` 接口
- 编译为 WASM 原型，可调用浏览器 API
- 元数据导出功能 (`.mimi.meta.json`)
- Swift/Kotlin 基础绑定生成器

### 6.4 长期 (v1.0+)
- 包管理器 `mimi add/remove/list` 支持带能力签名的包分发
- AI 驱动的胶水代码自动生成
- 在金融、隐私、边缘计算等领域的实际落地案例

---

## 7. 结论

Mimi 的胶水特性不是"又一个 FFI 实现"，而是一次安全范式的升级。它让跨语言调用从充满地雷的自由市场，变成有权限检查、有资源保障、有失败补偿的现代基础设施。

通过 `cap` 令牌，Mimi 在不改变任何现有语言的前提下，为每一次外部调用加上了编译期安全锁；通过内置的并发与补偿模型，它让多语言协作的事务性逻辑变得清晰可预测；通过意图元数据的保留与传递，它让 AI 工具和人类审查者能在异质系统中保持对代码行为的理解。

更重要的是，Mimi 自身可以被编译为高性能、类型安全的 C 库，并自动生成多种语言的绑定，将"安全胶水"的能力向外辐射——你的 Rust、Python、Swift 或 TypeScript 代码都可以安全地消费由 Mimi 编写的可信业务核心。

结合 Mimi 的四层渐进设计，你可以在项目的不同阶段灵活决定哪些部分保持模糊意图、哪些部分锁定为精确实现，在集成多语言时始终拥有可审计、可演进的安全基座。

---

> **实现状态说明**：本文档描述了 Mimi 语言的完整设计愿景。截至 v0.7.0，以下核心特性已实现并经过测试验证：
> - ✅ `cap` 线性类型与消费追踪
> - ✅ `extern "C"` 声明与解析
> - ✅ `on failure` 补偿块（scope-aware LIFO）
> - ✅ `parasteps` 并行执行（thread-based）
> - ✅ 基础类型检查与借用追踪
> - ✅ `mimi init/add/remove/list` 包管理
> - ✅ `mimi test` 测试框架
> - ✅ `mimi lsp` 基础 LSP 服务器
> - ✅ `&[u8]` 切片类型 (T702)
> - ✅ Actor codegen（结构体 + 构造函数）
> - ✅ Parasteps/Spawn/Await codegen（顺序执行回退）
> - ✅ Cap type 解析与 codegen
> - ✅ lexer()/parse() 运行时函数
> - ✅ Comptime 函数执行验证
> 
> 以下特性为设计目标，尚未实现：
> - ⏳ `unsafe` 块
> - ⏳ FFI 类型转换自动化
> - ⏳ WASM 编译目标
> - ⏳ 自动绑定生成器
> - ⏳ Trait vtable codegen（动态分派）
> - ⏳ 真正的并行执行（非顺序回退）