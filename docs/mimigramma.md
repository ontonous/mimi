# Mimi 完整语言愿景（v1.x / L4 聚焦）

> **范围说明**：本文档描述 Mimi 的长期、深度设计愿景，聚焦于 L4 生产模式与远期能力。当前 v1.0 基线以 `2.md` 中的决策和更新后的 `mimi.md` 为准；本文档中超出该基线的特性（如完整形式化验证、分布式 actor、效应系统等）将在 v1.0 之后分阶段实现。

**高信息密度、渐进可执行、契约驱动、人机协同的现代系统编程语言**

---

## 1. 设计理念重述

Mimi 的**唯一事实源**是从模糊意图到极致精确的连续光谱。  
`.mimis`（MimiSpec）定稿为 L1~L3 描述层，`.mimi`（Mimi）在 L4 层提供**零成本抽象的精确实现**，且两者语法兼容、文件可无缝演进。

本规范聚焦 **`.mimi` 生产模式**的深度设计，遵循以下核心承诺：

- **意图永存**：`desc`、`rule`、`math`、锁后缀在编译后的二进制中仍以元数据存在，驱动文档、审计和 AI 工具。
- **契约即类型**：`requires` / `ensures` 不只是注释，在特定编译模式下会被静态证明或动态检查。
- **零开销安全**：区域内存 (Arena)、线性能力、所有权借用，在不依赖 GC 的前提下消除内存错误。
- **结构化并发**：`parasteps` + `on failure` 提供原生的分布式 Saga 事务语义。
- **元编程安全**：`comptime` 只能操作卫生 AST，严禁 I/O，保证编译确定性。
- **人类意愿编码**：`$` / `$$` 锁由编译器强制执行，AI 辅助工具无权逾越。

---

## 2. 词法与语法强化

### 2.1 后缀锁的编译器语义
在 `.mimi` 中，标识符上的 `$`/`$$` 含义扩展：

| 后缀 | 编译期行为 | 工具行为 (IDE/AI) |
|------|-----------|-------------------|
| `$` | 该节点生成的代码/类型不可被编译器优化删除；若未提供 L4 实现则报错（在 `--strict` 下） | 禁止自动修改 |
| `$$` | 同上，且必须在项目中存在对应的“解锁声明”文件才允许被重构 | 完全只读 |
| `?` | 该节点接受 AI 建议，编译器不强制实现 | 允许 AI 生成替代方案 |
| `??` | 完全由 AI 决定，若未补全则编译为 `unimplemented!()` 桩 | AI 可自由修改 |

组合规则不变：先锁后疑，如 `$?` 表示“锁定但 AI 可以审视是否该锁”。

### 2.2 花括号体 vs 缩进体
- **花括号体** `{ ... }`：标准语句序列，是 L4 实现的主要载体。
- **缩进体**（以 `:` 换行缩进开始）：L3 步骤流或契约块，编译器内部将其转换为等价的控制流图 (CFG) 并进行静态分析，但如果没有花括号体，函数仍被视为“未完全实现”。
- **混合规则**：一个函数可以同时存在缩进的 `requires:`、`ensures:`、`math:`、`desc:` 和花括号体的实现。缩进部分必须在花括号体之前。
- `steps:` 块仅允许在缩进体中出现；若函数有花括号体，则不能再有 `steps:` 块，因为 L4 实现已经替代了步骤描述。

### 2.3 模块与文件
- 每个 `.mimi` 文件是一个模块（除非使用 `mod` 声明子模块）。
- 使用 `use` 导入，路径分隔 `::`，字段访问 `.`。
- 顶层可见性控制：`pub` 对外公开，默认私有。
- `module` 关键字作为命名空间容器，可以嵌套，内部使用花括号。
- 文件顶层的 `module` 声明不是必须的（没有时文件本身就是一个匿名根模块）。

---

## 3. 类型系统深化

### 3.1 代数数据类型 (ADT)
```mimi
type Shape {
    Circle(f64)            // 变体构造器带一个字段
    Rectangle(f64, f64)
    Triangle { a: f64, b: f64, c: f64 }
}
```
- 变体构造器可以是有名、匿名字段。
- `match` 表达式提供穷尽性检查。
```mimi
func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rectangle(w, h) => w * h,
        Triangle { a, b, c } => {
            let s = (a + b + c) / 2.0;
            (s * (s-a) * (s-b) * (s-c)).sqrt()
        }
    }
}
```

### 3.2 泛型
```mimi
func first<T>(list: &[T]) -> Option<&T> { ... }
type Pair<A, B> { first: A, second: B }
```
- 支持泛型参数在类型和函数上，使用尖括号。
- 可推导 trait/约束（未来扩展）：`where T: Numeric`。

### 3.3 特性 (Traits) / 接口

`interface` / `trait` 是 v1.1+ 的远期方向。v1.0 不引入 trait 系统，多态通过 duck typing 与 `comptime` 静态分派实现；如下示例展示的是未来可能的语法形态：

```mimi
interface Printable {
    func to_string() -> string;
}
impl Printable for Order {
    func to_string() -> string { ... }
}
```

### 3.4 类型别名与 Newtype
```mimi
type Meter = f64;          // 透明别名
newtype UserId = u64;      // 新类型，强类型隔离
```

### 3.5 模式匹配与解构
`match` 支持：字面量、变量绑定、通配符 `_`、构造器解构、守卫 `if`。

---

## 4. 内存模型深化

### 4.1 所有权传递
- **Move** 语义默认应用于所有非 `Copy` 类型。
- `let y = x;` 后 `x` 不可用（编译错误）。
- `Copy` 类型由编译器自动推导，或使用 `#[derive(Copy)]` 显式声明。
- 显式克隆：`let y = x.copy();`

### 4.2 借用与生命周期
- 不可变引用 `&T`，可变引用 `&mut T`，遵循独占规则。
- 生命周期省略：函数签名中编译器自动推导，参考 Rust 规则。
- 显式生命周期注解在复杂场景可用（如 `func longest<'a>(x: &'a str, y: &'a str) -> &'a str`）。但为了渐进性，`'a` 语法在初级可以省略，由编译器推断并提示。

### 4.3 共享所有权与弱引用
- `shared T`：原子引用计数，线程安全。内部可变性需使用 `Mutex<T>` 等。
- `local_shared T`：非原子引用计数，仅限单线程使用。
- `weak T`：从 `shared` 降级而来的弱引用，`upgrade()` 返回 `Option<shared T>`。
- 循环引用检测：编译器内置**静态循环引用分析器**，在调试构建中报告潜在泄漏；生产构建中可插入运行时弱引用降级建议。

### 4.4 Arena 区域内存
```mimi
func process(req: Request) -> Response {
    arena {
        let ref temp = build_huge_graph(req);
        let result = analyze(temp);
        // 返回时必须提取值，不能返回 ref
        Response { data: result.copy() }
    }
}
```
- `arena { }` 块内分配的所有 `ref T` 对象生命周期等于该块。
- 编译器在块出口自动回收整个 Arena。
- **禁止逃逸**：将 `ref` 赋给外层变量或传递给 `shared` 是编译错误。

---

## 5. 线性能力 (`cap`) 的完全规格

线性类型必须在每个控制流路径上被显式消费或传递。

```mimi
cap FileReadCap;   // 声明

func read_config(path: string, cap: FileReadCap) -> Result<Config, Err> {
    let data = std::fs::read_to_string(path)!;  // 使用 cap
    let config = parse(data)?;
    drop(cap);            // 显式消费 cap，表明权限释放
    Ok(config)
}
```
- `cap` 不可复制，不可丢弃（编译器报 `must_use`）。
- 若函数接受 `cap` 但不消费，必须在返回时将 `cap` 传回（如 `Ok(config, cap)`），或者将其 move 到子调用中。
- `drop` 关键字用于终止能力。
- 标准库提供基础 `cap` 类型：`FileReadCap`, `FileWriteCap`, `NetConnectCap` 等，注入到 main 函数或通过环境提供。

**能力组合**：
```mimi
cap FullFileAccess = FileReadCap + FileWriteCap;
```
使用 `+` 组合多个 cap 为一个，`&` 永远表示借用。消费时分解（示意）：
```mimi
let (r, w) = full_cap.split();
```

---

## 6. 并发模型深化

### 6.1 `actor` 完整语义
- 每个 `actor` 实例由运行时**轻量级任务**调度，不占用独立 OS 线程。
- 内部状态变量默认不可变；可变字段须加 `mut` 关键字。
- 方法调用是**异步消息**：调用者立即得到 `Future<T>`，需 `await` 取得结果。
- 消息保证 FIFO 顺序，但不同 actor 间无全局顺序。
- 死锁预防：编译期无法完全禁止循环 await，但运行时提供超时检测，开发模式可报告潜在死锁。

```mimi
actor BankAccount {
    mut balance: f64 = 0.0;

    func deposit(amount: f64) {
        self.balance += amount;
    }
    func withdraw(amount: f64) -> Result<f64, string> {
        if self.balance >= amount {
            self.balance -= amount;
            Ok(amount)
        } else {
            Err("insufficient funds")
        }
    }
}
// 使用
let acc = BankAccount.spawn();
await acc.deposit(100.0);
let cash = await acc.withdraw(30.0)?;
```

### 6.2 `parasteps` 的底层实现
- `parasteps` 块被编译为一个并发作用域，使用**结构化并发**原语（类似 Kotlin 协程或 Swift 任务组）。
- 内部每一步是 `spawn` 表达式，产生 `Future`。
- 所有子任务并发执行；父任务在 `parasteps` 结尾处 `await` 所有子任务。
- 若任一子任务失败（抛出错误），取消所有未完成的子任务（Cancel Token 传播）。
- 取消是**协作式**的：子任务在 `await` 点检查取消信号。
- 失败补偿规则见下一节。

---

## 7. 结构化补偿与 Saga 协议

### 7.1 补偿注册与执行
`on failure { ... }` 块被注册到当前作用域的**补偿栈**。退出作用域时若未触发失败，补偿被丢弃；若触发（异常或 `error`），则按 LIFO 逆序执行补偿。

```mimi
func booking() -> Result<(), Err> {
    let seat = reserve_seat()?;
    on failure { cancel_seat(seat); }

    let hotel = book_hotel()?;
    on failure { cancel_hotel(hotel); }

    // 如果 payment 失败：
    // 1. cancel_hotel(hotel)
    // 2. cancel_seat(seat)
    let payment = charge()?;  
    on failure { refund(payment); }

    Ok(())
}
```
- 补偿块本身也是一个作用域，内部可以再次注册补偿。
- 补偿块执行失败时，错误累积为 `CompositeError`，原错误永远为主错误。

### 7.2 `parasteps` 内的补偿传播
- 每个 `spawn` 内部可能有自己的 `on failure`。
- 当取消发生时，先等待所有子任务完成其局部补偿（或超时），然后 `parasteps` 整体向外抛出错误，触发外部补偿。
- 开发者可指定超时策略：`parasteps timeout 5s { ... }`，超时视为失败。

---

## 8. 契约与数学的 L4 集成

### 8.1 契约检查的编译模式
- `mimi build`（默认）：仅对可静态求值且不涉及外部状态的 `requires`/`ensures` 进行检查（如常量表达式、简单算术比较）。自然语言 `rule` 被忽略。
- `mimi build --verify-contracts`：使用 SMT 求解器尝试证明部分契约（如整数、集合、形状），并将结果以 warning/error 报告。
- `mimi build --strict`：所有未被 `$`/`$$` 锁定的 `rule` 必须通过 AI 审计或形式化证明，否则编译失败。

### 8.2 `old()` 的实现
- 函数入口自动对所有在 `ensures` 中被 `old()` 引用的变量进行**快照**。
- 对于 `Copy` 类型，快照无开销。
- 对于非 `Copy` 类型，编译器生成隐式 `copy()` 调用，并**强制发出 Warning**（不可抑制），提醒开发者注意性能。
- 开发者可手动优化：自行管理快照变量（如 `let old_balance = account.balance;`），然后在 `ensures` 中使用 `old_balance` 而非 `old(account.balance)`。此时不报警告。

### 8.3 `math:` 块的代码生成潜力
`math:` 块不仅是文档，在以下场景会被利用：
- **形状推导**：用于张量运算时，编译器可自动推断输出形状，避免运行时检查。
- **编译期求值**：若 `math` 块中的表达式完全由常量组成，编译器可将其视为编译期常量。
- **微分域**：对于标记为 `#[differentiable]` 的函数，`math` 块可为自动微分提供指导。

---

## 9. 编译期元编程 (Comptime) 安全边界

### 9.1 卫生 AST 操作
- v1.0 中 `comptime` 裁剪为仅支持 `quote!` 宏生成；完整的类型元数据反射 (`type_info(T)`) 是 v1.1+ 方向。
- `comptime` 块内运行在编译器 VM 中，输入是 AST 节点（v1.1+ 可扩展为类型元数据），输出是 AST 片段。
- 所有代码生成通过 **AST 拼接**，严禁字符串拼接。
- 示例：
```mimi
comptime func generate_getters(T: type) -> AST {
    let meta = type_info(T);
    let mut block = ast::Block::new();
    for field in meta.fields.iter().filter(|f| f.visibility == Pub) {
        let getter = quote! {
            pub func get_$(field.name)(self: &Self) -> &$(field.ty) {
                &self.$(field.name)
            }
        };
        block.push(getter);
    }
    block
}

// 使用
#[generate_getters]
type User {
    pub name: string,
    pub age: u32,
}
```
- `quote!{}` 是卫生的，内部变量不会与外部冲突。

### 9.2 I/O 隔离
- `comptime` 代码**不能**调用任何执行 I/O 的标准库函数。违者编译错误。
- 若要读取配置文件，必须使用内置宏 `comptime_read_file!("path")`，该宏返回字符串常量，且文件被构建系统跟踪依赖。
- 确定性保证：`comptime` 内禁止随机数、当前时间等非确定性操作。

---

## 10. 错误处理统一模型

Mimi 的错误处理融合了 `Result`、`Option`、`error` 语句和异常补偿。

- **可恢复错误**：`Result<T, E>` + `?` 运算符。
- **不可恢复/终止**：`error "msg" to exit`，此时会触发已注册的 `on failure` 补偿，然后传播到根，终止当前任务（或 actor）。
- **显式放弃**：`drop(result)` 忽略 `Result` 值和其潜在的补偿责任。如果 `Result` 的 `E` 类型有 `must_use` 属性，`drop` 也会触发警告，需显式写出。
- **Option**：`T?` 语法糖，`?` 运算符可自动传播 `None`（在返回 `Option` 的函数中）。

---

## 11. 包管理与模块系统

### 11.1 项目结构
```
my_project/
├── mimi.toml           # 包配置
├── src/
│   ├── main.mimi         # 入口
│   ├── domain.mimi       # 领域模块
│   └── services/
│       └── payment.mimi
├── tests/
│   └── integration.mimi
└── sketches/           # .mimis 草图文件（可选，构建时忽略或仅校验）
    └── design.mimis
```

### 11.2 mimi.toml 示例
```toml
[package]
name = "shop"
version = "0.1.0"

[dependencies]
std = "1.0"
payment-sdk = { path = "../payment-sdk" }
```

### 11.3 构建与运行
- `mimi build`：编译整个包，输出可执行文件或库。
- `mimi check`：仅进行语法和类型检查，适用于草图模式也适用于 `.mimi`。
- `mimi promote <file>`：将 `.mimis` 转换为 `.mimi`（实际是重命名，并清理一些 `.mimis` 特有的语法提示）。
- `mimi doc`：提取 `desc` 和签名生成文档（可输出 `.mimis` 或 Markdown）。
- `mimi test`：运行测试。

---

## 12. 与 MimiSpec 的自洽细节

- **锁后缀**：`.mimis` 中的 `$` 进入 `.mimi` 后继续生效，编译器控制工具行为。
- **`@import` 兼容**：`.mimi` 遇到 `@import "file.mimis"` 时，会尝试加载该文件并解析为模块，允许 `.mimis` 中的类型和函数声明被引用（但 `.mimis` 文件不会被编译为代码，仅作为接口定义）。
- **`desc` 作为实体**：在 `.mimi` 中，`desc` 仍然是合法实体，它会被存储为元数据，并可由 `mimi doc` 提取。在花括号体内，`desc` 也可以作为语句出现（语义为“此处意图说明”），但不对应任何运行时代码。
- **`rule` 编译模式**：`.mimi` 中的 `rule` 默认不产生编译检查，除非使用 `--verify-rules` 或 `--strict`。

---

## 13. 未来方向与开放问题

1. **形式化验证集成**：`--verify-contracts` 目前只是规划，需选定 SMT 后端（如 Z3）。
2. **分布式 Actor**：actor 目前仅限进程内，未来可透明远程化。
3. **编译期反射与序列化**：通过 `comptime` 实现零开销序列化框架。
4. **效应系统**：将 `cap` 扩展为完整的代数效应，标记函数的副作用（纯、抛出、异步等）。
5. **LSP 与 IDE 支持**：需在语言设计稳定后实现语言服务器。

---

Mimi 的长期目标是让“从模糊意图到精确代码”的每一步都有编译器与工具参与，使人类与 AI 在锁与不确定的边界上持续协作。