# Mimi 多语言胶水特性文档

> **版本**：v0.2.0  
> **最后更新**：2026-06-16  
> **状态**：草案，部分功能已实现，持续演进

---

## 1. 设计理念

Mimi 是一门**系统级胶水语言**，旨在成为连接不同语言、运行时和生态的“安全中枢”。它的核心设计目标不是取代 C、C++、Rust、TypeScript 或 Python，而是**以静态可验证的方式无缝调度它们**。

传统的胶水语言（如 Python）在快速集成上有巨大优势，但往往存在以下问题：
- 缺乏编译期安全检查（内存错误、类型不匹配、资源泄漏）。
- 并发和事务处理需要手工编码，极易出错。
- 难以对 AI 生成的代码建立信任，因为无法静态审计权限和契约。

Mimi 通过以下机制解决这些问题：
- **能力型 FFI**：基于线性能力类型 (`cap`) 对外部调用进行静态权限管控。
- **内置并发与补偿**：`parasteps` 与 `on failure` 提供声明式的并发与 Saga 事务，极大简化胶水代码。
- **四层渐进**：支持从模糊意图（`desc`）到生产实现（`$`）的平滑演进，特别适合 AI 辅助开发。

**一句话定位**：Mimi 是“具有安全护栏的终极胶水”——像 Python 一样便捷集成，但提供 Rust 级别的内存与并发安全。

---

## 2. 核心架构：基于 `cap` 的能力型 FFI

Mimi 不采用传统“直接暴露所有 C 函数”的 FFI 方式。取而代之的是**能力令牌 (Capability Token)** 模式。

### 2.1 能力类型 (`cap`)

`cap` 是一种**线性类型**，代表执行特定外部操作的权利。它具有以下性质：
- **不可复制**：不能通过赋值创建多个实例。
- **不可隐式丢弃**：必须被显式消费（传递给函数或 `drop`）。
- **必须传递**：调用敏感外部函数时，必须在参数中显式传入对应的 `cap`。

这种设计允许编译器在编译期静态追踪：谁有权访问文件系统？哪个模块可以发起网络请求？权限边界清晰且无法绕过。

### 2.2 FFI 声明

外部函数通过 `extern` 块声明，并明确标注所需的 `cap`。

```mimi
// 定义一个代表“文件系统写入权限”的能力
cap FileWriteCap;

// 引入外部 C 库的函数，要求传入 FileWriteCap
extern "C" {
    fn write_to_file(path: string, data: &[u8], cap: FileWriteCap) -> Result<(), IOErr>;
}
```

### 2.3 权限传递与检查

- 只有创建或获取到 `FileWriteCap` 的代码才能调用 `write_to_file`。
- 编译器在每个调用点检查 `cap` 是否有效、未被消费。
- 若尝试在未持有能力的情况下调用，编译失败，并给出明确错误：“缺少能力 `FileWriteCap`”。

### 2.4 `unsafe` 边界与信任降级

对于确实需要绕过安全检查的底层操作，Mimi 提供 `unsafe` 块。`unsafe` 内可以调用未声明 `cap` 的外部函数，但必须由开发者手工审计。该块的代码在编译报告中被标记，AI 在 `--verify-rules` 时会重点审查。

---

## 3. 支持的语言与运行时

Mimi 的胶水设计采用**分层接入**策略，优先级排序如下：

### 3.1 第一阶段：C ABI 直达（当前实现）

- **C**：通过标准 C ABI 调用任何 C 动态/静态库。
- **C++**（通过 `extern "C"` 封装）、**Rust**（`extern "C"` 或 `cbindgen` 导出）、**Zig** 等所有可暴露 C ABI 的语言。
- Mimi 自身也可被编译为 C 动态库，供其他语言调用。

### 3.2 第二阶段：WASM 互操作（计划中）

- 将 Mimi 编译为 WebAssembly，与 JS/TS 生态互操作。
- 在浏览器或 Edge 环境中成为“安全胶水”，调用 JS API 需通过 `cap` 授权。

### 3.3 第三阶段：原生绑定生成器（路线图）

- 提供工具，从 C 头文件或 Rust/TS 类型定义自动生成 Mimi 的 `extern` 声明与 `cap` 包装。
- 目标：用户只需在 MimiSpec Editor 中搜索“redis client”，即可获得类型安全、权限受控的 Mimi 接口。

---

## 4. 使用示例

### 4.1 安全调用 C 库（SQLite 示例）

```mimi
cap SQLiteCap;

extern "C" {
    fn sqlite3_open(path: string, cap: SQLiteCap) -> Result<SqliteDB, SqliteErr>;
    fn sqlite3_exec(db: &SqliteDB, query: string, cap: SQLiteCap) -> Result<(), SqliteErr>;
}

func init_db(path: string, cap: SQLiteCap) -> Result<SqliteDB, SqliteErr> {
    let db = sqlite3_open(path, cap)?;
    // cap 仍可用，因为传递的是引用语义？这里需消费能力：通常 cap 作为借用或所有权传递。
    // 实际设计中，cap 可作为类似 &cap 借用，或通过函数签名明确消耗。
    // 此处仅示意。
    Ok(db)
}
```

> 注：能力传递的精确语义（借用 vs 移动）在 v0.3 中细化。

### 4.2 并发调度多个服务（胶水核心）

```mimi
func sync_user_data(user_id: u64, fs_cap: FileSysCap, net_cap: NetworkCap) -> Result<(), Err> {
    // 并行执行：从文件加载 + 调用外部 API
    let (local_data, remote_data) = parasteps "加载用户数据" {
        let p = spawn load_from_disk(user_id, fs_cap);
        let q = spawn fetch_from_api(user_id, net_cap);
        await (p, q)
    }!;

    // 合并数据，写入文件（使用 fs_cap）
    store_combined(user_id, local_data, remote_data, fs_cap)
}
```

若 `fetch_from_api` 失败，`parasteps` 会自动取消 `load_from_disk`（若还在进行），并触发各自作用域内注册的 `on failure` 补偿，外部调用者只需处理最终错误。

### 4.3 与 Python 生态交互（通过 C 桥接）

Mimi 不直接执行 Python 字节码，但可以调用 CPython 的 C API，利用 `PythonCap` 限制解释器实例化。

```mimi
cap PythonCap;

extern "C" {
    fn py_import(module: string, cap: PythonCap) -> Result<PyModule, PyErr>;
    fn py_call(mod: PyModule, func: string, args: &[PyValue], cap: PythonCap) -> Result<PyValue, PyErr>;
}

func ai_classify(text: string, py_cap: PythonCap) -> Result<string, Err> {
    let mod = py_import("transformers", py_cap)?;
    let result = py_call(mod, "classify", &[PyValue::String(text)], py_cap)?;
    Ok(result.to_string())
}
```

这种设计使得在需要时仍然可以借助 Python 庞大的 AI 生态，但调用被限制在持有 `PythonCap` 的授权模块内，AI Agent 的恶意代码无法擅自启动 Python 解释器。

---

## 5. 胶水语言的对比分析

| 特性 | Python | Lua | Nim | Rust | **Mimi** |
|------|--------|-----|-----|------|-----------|
| **FFI 安全性** | 无静态检查 | 无 | 部分 | 高（但需 `unsafe`） | **极高（cap 权限+类型安全）** |
| **编译产物** | 解释执行 | 解释/JIT | 原生 | 原生 | **原生** |
| **并发模型** | GIL / asyncio | 单线程 | 线程/异步 | 线程/async | **parasteps + actor** |
| **事务补偿** | 手工 try/finally | 手工 | 手工 | 手工 | **语言内置 on failure** |
| **AI 可审计性** | 弱 | 弱 | 中 | 中 | **强（desc / --verify-rules）** |
| **资源管理** | GC + 手工 | GC | GC + 可选 | RAII + 借用 | **线性类型 + 隐式 Move** |
| **包生态** | 极丰富 | 小 | 小 | 成长中 | **初期（通过 C 桥接复用）** |
| **学习曲线** | 低 | 低 | 中 | 高 | **中（需理解 cap / 补偿）** |

Mimi 不是要颠覆 Python 的生态，而是**在需要安全、并发和事务可靠性的集成点上，提供比 Python 更可信的选择**。对于快速原型或无安全顾虑的脚本，Python 依然合适；但当代码将部署到生产环境、处理金钱或隐私、或由 AI 自动生成时，Mimi 是更负责任的胶水。

---

## 6. 当前局限与路线图

### 6.1 已实现 (v0.2.0)
- `cap` 线性类型及消耗追踪
- `extern "C"` 声明解析
- 树遍历解释器支持基本 FFI 调用（概念验证）

### 6.2 短期 (v0.3.0)
- 完善 `cap` 在 FFI 中的所有权语义（借用 vs 移动）
- 接通 `on failure` 自动执行，确保资源安全清理
- 提供至少 3 个关键 C 库的 Mimi 封装（如 SQLite、libcurl、TensorFlow Lite）

### 6.3 中期 (v0.5.0)
- 自动 FFI 绑定生成器，从 `.h` 文件生成 `cap` 包装
- MimiSpec 中的外部能力依赖声明
- 初步 WASM 编译目标，与 JS/TS 生态互操作

### 6.4 长期 (v1.0)
- 包管理器 `mipm` 支持能力签名的包安装
- 基于 OSE 的智能 Agent 自动编写胶水代码
- 安全胶水在企业级应用中的实际案例沉淀

---

## 7. 结论

Mimi 的胶水特性不是“另一个 FFI”，而是一次安全范式的升级。它让跨语言调用从充满地雷的自由市场，变成有权限检查、有资源保障、有失败补偿的现代基础设施。结合 Mimi 的四层渐进和 AI 审计能力，你将能够以前所未有的速度和信心，组装出一个由多种语言协作而成的可靠系统。