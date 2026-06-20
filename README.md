# Mimi 语言

**Mimi** 是一门面向 **Intent-as-Code + Safe AI Collaboration** 的系统编程语言。

它的核心目标不是单纯的性能或表达力，而是把"人类已锁定的决策"与"AI 可生成的未确定区域"之间的边界，变成编译器、IDE 和构建工具可以直接执行的一等语言构造。

---

## 特性一览

| 特性 | 状态 | 说明 |
|------|------|------|
| 基本类型 | ✅ | `i32`, `i64`, `f64`, `bool`, `string`, `unit`, `nothing` |
| 函数与闭包 | ✅ | `func` 命名函数, `fn` 匿名闭包, 一等函数 |
| ADT + 模式匹配 | ✅ | 枚举、记录、元组, `match` 穷尽性检查 |
| 泛型 | ✅ | `<T>` 类型参数, where 约束, Turbofish |
| Move 语义 | ✅ | Copy trait, use-after-move 检测 |
| 借用检查 | ✅ | `&T` / `&mut T` 类型检查层面的借用规则 |
| `newtype` | ✅ | 强类型隔离包装 |
| `type A = B` | ✅ | 透明类型别名 |
| Actor 并发 | ✅ | 轻量任务调度, 同步方法调用 |
| `parasteps` 并发 | ✅ | `spawn` / `await`, 线程池并行 |
| `on failure` 补偿 | ✅ | LIFO 逆序执行补偿块 |
| `requires` / `ensures` | ✅ | 运行时契约断言, `result` 变量 |
| `cap` 线性能力 | ✅ | 类型检查层面的能力追踪 |
| `desc` / `rule` 元数据 | ✅ | 意图描述与约束声明 |
| 复合赋值运算符 | ✅ | `+=`, `-=`, `*=`, `/=` |
| 字符串操作 | ✅ | 拼接 `+`, `len()`, `to_string()`, `contains()` |
| 内置函数 | ✅ | `abs`, `min`/`max`, `push`/`pop`, `range`, `sqrt`, `input` |
| 意图后缀 | ✅ | `$`, `$$`, `?`, `??` 锁定与委托标记 |
| `pub` 可见性 | ✅ | 函数、类型、Actor 的公开标记 |
| `old()` in ensures | ✅ | 函数入口变量快照语义 |
| `trait` / `impl` | ✅ | 基础 trait 系统与静态分派 |
| `where` 约束 | ✅ | 泛型类型约束语法 |
| `extern "C"` | ✅ | FFI 块声明外部函数 |
| 标准库 | ✅ | prelude, io, fs, strings, collections, mymath, net, maps, json, time, datetime, env, testing, random, text, result |

---

## 快速开始

### 安装

```bash
cd mimi
cargo build --release
```

### 编写 Mimi 程序

创建 `hello.mimi`:

```mimi
func greet(name: string) -> string {
    "Hello, " + name + "!"
}

func main() -> i32 {
    println(greet("World"));
    0
}
```

### 运行

```bash
# 类型检查
./target/release/mimi check hello.mimi

# 运行
./target/release/mimi run hello.mimi
```

---

## 语法示例

### 函数与闭包

```mimi
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    ensures: result == a + b
    a + b
}

func main() -> i32 {
    let double = fn(x: i32) -> i32 { x * 2 };
    let values = [1, 2, 3, 4, 5];
    let mut sum = 0;
    for v in values {
        sum += double(v);
    }
    sum
}
```

### 泛型

```mimi
pub func find<T>(xs: List<T>, target: T) -> (bool, i32) {
    for i in range(0, len(xs)) {
        if xs[i] == target {
            return (true, i)
        }
    }
    (false, -1)
}

pub func map_list<T, U>(xs: List<T>, f: func(T) -> U) -> List<U> {
    reduce(xs, fn(acc: List<U>, x: T) -> List<U> {
        push(acc, f(x))
    }, [])
}
```

### ADT 与模式匹配

```mimi
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
    Triangle { a: f64, b: f64, c: f64 }
}

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14 * r * r,
        Rectangle(w, h) => w * h,
        Triangle { a, b, c } => {
            let s = (a + b + c) / 2.0;
            (s * (s-a) * (s-b) * (s-c)).sqrt()
        }
    }
}
```

### 错误处理

```mimi
func divide(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 {
        Err("division by zero")
    } else {
        Ok(a / b)
    }
}

func safe_calc() -> Result<i32, string> {
    let result = divide(10, 2)?;
    Ok(result * 3)
}
```

### 并发

```mimi
func fetch(url: string) -> string { /* ... */ }

func main() -> string {
    let result = "";
    parasteps {
        let a = spawn fetch("api/users");
        let b = spawn fetch("api/orders");
        let r1 = await a;
        let r2 = await b;
        result = r1 + r2
    }
    result
}
```

### 补偿

```mimi
func booking() -> Result<(), string> {
    let seat = reserve_seat()?;
    on failure { cancel_seat(seat) }

    let hotel = book_hotel()?;
    on failure { cancel_hotel(hotel) }

    Ok(())
}
```

---

## CLI 命令

| 命令 | 说明 |
|------|------|
| `mimi check <file.mimi>` | 类型检查 `.mimi` 文件 |
| `mimi run <file.mimi>` | 类型检查并运行 |
| `mimi build <file.mimi>` | 编译为本地可执行文件 |
| `mimi build <file.mimi> --verify-contracts` | 编译并验证合约 |
| `mimi fmt <files...>` | 格式化文件 |
| `mimi fmt --check <files...>` | 检查格式（不修改） |
| `mimi lint <files...>` | 静态分析 |
| `mimi lsp` | 启动 LSP 服务器 |
| `mimi init` | 初始化新项目 |
| `mimi add <name>` | 添加依赖 |
| `mimi install` | 安装依赖 |
| `mimi tree` | 显示依赖树 |
| `mimi list` | 列出依赖 |
| `mimi publish` | 发布到本地 registry |

---

## 项目结构

```
mimi/
├── src/
│   ├── main.rs          # CLI 入口
│   ├── ast.rs           # AST 类型定义
│   ├── lexer.rs         # 词法分析器
│   ├── parser/          # 语法分析器
│   ├── core/            # 类型检查器
│   ├── interp/          # 解释器
│   ├── codegen/         # LLVM codegen
│   │   ├── mod.rs
│   │   └── builtins.rs
│   ├── lsp.rs           # LSP 服务器
│   ├── fmt.rs           # 格式化器
│   ├── lint.rs          # 静态分析
│   ├── diagnostic/      # 诊断系统
│   └── tests/           # 测试套件
├── std/                 # 标准库
│   ├── prelude.mimi
│   ├── io.mimi
│   ├── fs.mimi
│   ├── strings.mimi
│   ├── collections.mimi
│   ├── mymath.mimi
│   ├── net.mimi
│   ├── maps.mimi
│   ├── json.mimi
│   ├── time.mimi
│   ├── env.mimi
│   └── testing.mimi
├── examples/            # 示例程序
└── docs/                # 文档
    ├── syntax-reference.md   # 语法规范 (★)
    ├── mimi.md               # 语言设计规范
    └── ...
```

---

## 快速上手：5 分钟 FFI 调用

调用 C 标准库 `strlen`，从零到运行：

```mimi
// 1. 声明外部函数
extern "C" {
    func strlen(s: string) -> i64;
}

// 2. 在主函数中调用
func main() {
    let len = strlen("Hello from Mimi FFI!")
    println("字符串长度:", len)
}
```

```bash
mimi run demo.mimi   # 输出: 字符串长度: 22
```

更多 FFI 示例见 [readme/10-ffi.md](readme/10-ffi.md)。

---

## 文档导航

| 文档 | 说明 | 适合读者 |
|------|------|----------|
| [syntax-reference.md](docs/syntax-reference.md) | **语法规范** — 基于解析器实现 | 所有 Mimi 开发者 |
| [mimi.md](docs/mimi.md) | 语言设计规范 v1.0 | 语言实现者、贡献者 |
| [design-decisions.md](docs/design-decisions.md) | 设计决策与语言对比 | 想了解"为什么这样设计"的人 |
| [future-vision.md](docs/future-vision.md) | 长期愿景 v1.x/L4 | 核心开发者、架构师 |
| [ffi-glue.md](docs/ffi-glue.md) | FFI 与多语言胶水 | 需要跨语言集成的开发者 |
| [product-strategy.md](docs/product-strategy.md) | 产品与开源策略 | 项目管理者、投资者 |

---

## 版本

当前版本: **v0.7.0**

语言规范: v1.0.0-rc.1

### 更新日志

- **v0.7.0** — Std 补齐, Codegen 修复, 工具链 (fmt/lint), 合约验证, 包管理升级
- **v0.3.1** — 基础类型、函数、闭包、ADT、模式匹配、错误处理
- **v0.1.1** — 初始版本

---

## 许可证

MIT
