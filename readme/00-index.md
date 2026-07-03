# Mimi 语言文档

> Mimi 是一门面向 **Intent-as-Code + Safe AI Collaboration** 的系统编程语言。
> 它将"人类已锁定的决策"与"AI 可生成的未确定区域"之间的边界，变成编译器、IDE 和构建工具可以直接执行的一等语言构造。

---

## 文档导航

| 文档 | 说明 | 适合读者 |
|------|------|----------|
| [01-语法手册](./01-syntax.md) | 完整的语法参考：关键字、运算符、表达式、语句 | 所有 Mimi 开发者 |
| [02-类型系统](./02-types.md) | 基础类型、ADT、泛型、trait、newtype、模式匹配 | 所有 Mimi 开发者 |
| [03-内存模型](./03-memory.md) | Move、借用、shared/weak、Arena、cap 线性能力 | 中级以上开发者 |
| [04-并发模型](./04-concurrency.md) | Actor、parasteps、spawn/await、on failure 补偿 | 中级以上开发者 |
| [05-错误处理](./05-error-handling.md) | Result、? 运算符、on failure、Saga 补偿 | 所有 Mimi 开发者 |
| [06-模块与包管理](./06-modules.md) | use 导入、模块系统、mimi.toml 包管理 | 所有 Mimi 开发者 |
| [07-CLI 参考](./07-cli.md) | 所有 CLI 命令详解 | 所有 Mimi 开发者 |
| [08-示例集](./08-examples.md) | 覆盖所有特性的完整代码示例 | 所有 Mimi 开发者 |
| [09-MimiSpec 集成](./09-mms-integration.md) | mms {} 块、意图嵌入、契约绑定 | AI 协作开发者 |
| [10-FFI 与跨语言](./10-ffi.md) | extern "C"、cap 授权、跨语言调用 | 需要跨语言集成的开发者 |

---

## 快速开始

### 安装

```bash
cd mimi
cargo build --release
# 可执行文件在 target/release/mimi
```

### Hello World

创建 `hello.mimi`：

```mimi
func main() -> i32 {
    println("Hello, Mimi!");
    0
}
```

运行：

```bash
./target/release/mimi run hello.mimi
```

### 核心概念速览

```mimi
// 1. 函数与契约
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    a + b
}

// 2. ADT 与模式匹配
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
}

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14 * r * r,
        Rectangle(w, h) => w * h
    }
}

// 3. 闭包与高阶函数
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5];
    let doubled = map(nums, fn(x: i32) -> i32 { x * 2 });
    println(doubled);
    0
}

// 4. 错误处理
func risky() -> Result<i32, string> {
    let value = may_fail()?;
    Ok(value)
}

// 5. 并发
func main() -> string {
    parasteps {
        let a = spawn fetch("api/users");
        let b = spawn fetch("api/orders");
        let r1 = await a;
        let r2 = await b;
        r1 + r2
    }
}

// 6. Actor
actor Counter {
    mut count: i32 = 0;
    func increment() { self.count += 1; }
    func get() -> i32 { self.count }
}

// 7. 意图后缀（仅 MimiSpec .mms，不是 Mimi .mimi 语法）
//    MimiSpec 示例：
//    func$$ critical_logic() { ... }   // 强锁定：AI 不得修改
//    func? maybe_change() { ... }      // 不确定：AI 可审视
```

---

## 语言特性总览

| 特性 | 状态 | 关键语法 |
|------|------|----------|
| 基本类型 | ✅ | `i32`, `i64`, `f64`, `bool`, `string`, `unit`, `nothing` |
| 函数与闭包 | ✅ | `func`, `fn`, 一等函数 |
| ADT + 模式匹配 | ✅ | `type`, `match`, 穷尽性检查 |
| Move 语义 | ✅ | Copy trait, use-after-move 检测 |
| 借用检查 | ✅ | `&T` / `&mut T` |
| 泛型 | ✅ | `func foo<T>(x: T) -> T` |
| Trait / Impl | ✅ | `trait Display`, `impl Display for T` |
| `where` 约束 | ✅ | `func foo<T>(x: T) where T: Display` |
| `newtype` | ✅ | `newtype UserId = u64` |
| Actor 并发 | ✅ | `actor`, `.spawn()`, `await` |
| `parasteps` | ✅ | `spawn`, `await`, 结构化并发 |
| `on failure` | ✅ | LIFO 补偿栈 |
| `requires` / `ensures` | ✅ | 契约断言, `old()`, `result` |
| `cap` 线性能力 | ✅ | `cap`, `.split()`, `drop()` |
| `comptime` | ✅ | `comptime func`, `quote!` |
| `mms {}` 块 | ✅ | MimiSpec 嵌入 |
| 意图后缀 | ✅（MimiSpec） | `$`, `$$`, `?`, `??`；仅 `.mms`，非 `.mimi` 语法 |
| `extern "C"` | ✅ | FFI 块 + LLVM codegen |
| `pub` 可见性 | ✅ | 函数、类型、Actor |
| 列表推导 | ✅ | `[expr for x in list]` |
| 分配器 | ✅ | `alloc(Arena)`, `alloc(Bump)`, `alloc(System)` |
| 标准库 | ✅ | `std::prelude`, `std::io`, `std::mymath`, `std::collections`, `std::strings`, `std::fs`, `std::random`, `std::text`, `std::result`, `std::maps`, `std::json`, `std::time`, `std::datetime`, `std::net`, `std::env`, `std::testing` |
| LSP 支持 | ✅ | 诊断、补全、悬停、跳转、引用、重命名、签名帮助、语义 token |
