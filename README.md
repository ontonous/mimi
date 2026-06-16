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
| `comptime` 关键字 | ✅ | 编译期元编程预留 |
| 复合赋值运算符 | ✅ | `+=`, `-=`, `*=`, `/=` |
| 字符串操作 | ✅ | 拼接 `+`, `len()`, `to_string()`, `contains()` |
| 内置函数 | ✅ | `abs`, `min`/`max`, `push`/`pop`, `range`, `sqrt`, `input` |
| 意图后缀 | ✅ | `$`, `$$`, `?`, `??` 锁定与委托标记 |
| `pub` 可见性 | ✅ | 函数、类型、Actor 的公开标记 |
| `cap.split()` | ✅ | 组合能力分解为独立能力 |
| `old()` in ensures | ✅ | 函数入口变量快照语义 |
| `math:` 块 | ✅ | 编译时常量表达式求值 |
| `trait` / `impl` | ✅ | 基础 trait 系统与静态分派 |
| `where` 约束 | ✅ | 泛型类型约束语法 |
| `extern "C"` | ✅ | FFI 块声明外部函数 |

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

### Move 语义

```mimi
func main() -> i32 {
    let x = 42;       // x 是 Copy 类型 (i32)
    let y = x;        // 复制，x 仍可用
    x + y             // OK: 84

    let s = "hello";  // s 是 Move 类型 (string)
    let t = s;        // s 被移动
    // s              // 错误: use of moved value
    1
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

### 错误处理与补偿

```mimi
type Res {
    Ok(i32)
    Err(string)
}

func booking() -> Res {
    let seat = reserve_seat()?;
    on failure { cancel_seat(seat); }

    let hotel = book_hotel()?;
    on failure { cancel_hotel(hotel); }

    let payment = charge()?;
    on failure { refund(payment); }

    Ok(1)
}
```

---

## CLI 命令

| 命令 | 说明 |
|------|------|
| `mimi check <file.mimi>` | 类型检查 `.mimi` 文件 |
| `mimi run <file.mimi>` | 类型检查并运行 |

---

## 项目结构

```
mimi/
├── src/
│   ├── main.rs      # CLI 入口
│   ├── ast.rs       # AST 类型定义
│   ├── lexer.rs     # 词法分析器
│   ├── parser.rs    # 语法分析器
│   ├── core.rs      # 类型检查器
│   ├── interp.rs    # 解释器
│   └── tests.rs     # 测试套件
└── docs/
    ├── mimi.md                # 语言规范 (v1.0)
    ├── design-decisions.md    # 设计决策与评估
    ├── future-vision.md       # 长期愿景 (v1.x/L4)
    ├── ffi-glue.md            # FFI 与多语言胶水特性
    └── product-strategy.md    # 产品策略与开源策略
```

---

## 文档导航

| 文档 | 说明 | 适合读者 |
|------|------|----------|
| [mimi.md](docs/mimi.md) | 语言规范 v1.0 | 语言实现者、贡献者 |
| [design-decisions.md](docs/design-decisions.md) | 设计决策与语言对比 | 想了解"为什么这样设计"的人 |
| [future-vision.md](docs/future-vision.md) | 长期愿景 v1.x/L4 | 核心开发者、架构师 |
| [ffi-glue.md](docs/ffi-glue.md) | FFI 与多语言胶水 | 需要跨语言集成的开发者 |
| [product-strategy.md](docs/product-strategy.md) | 产品与开源策略 | 项目管理者、投资者 |

---

## 版本

当前版本: **v0.1.1**

语言规范: v1.0 基线整合版

---

## 许可证

MIT
