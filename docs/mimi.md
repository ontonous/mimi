# Mimi 语言 (.mimi) 核心设计规范 v1.0

Mimi 是面向**契约驱动生产编译**的系统编程语言。它是 MimiSpec 意图描述语言（`.mms`）的编译后端，专注于可执行正确性、结构化并发和线性能力。

Mimi 不与 Rust 比拼极限裸性能，也不与 Python 比拼原型速度，而是在 **"可验证的合约正确性 + 结构化并发安全"** 这一维度上建立差异。

---

## 1. 设计支柱

### 1.1 生产优先

Mimi 只处理可编译执行的精确代码。渐进开发（草图→意图→蓝图→生产）是 MimiSpec 的职责，通过 `mms {}` 块与 Mimi 建立追溯链接。

### 1.2 合约即代码

`requires` / `ensures` / `old()` 作为一等语言构造内嵌在函数签名中，不是外部注解。v1.0 以运行时断言为主，Z3 SMT 验证通过 `--verify-contracts` 启用。

### 1.3 结构化并发与安全

- `parasteps` 块内的 `spawn`/`await` 提供确定性并行
- `on failure` 补偿栈提供 LIFO 事务安全
- `cap` 线性能力提供权限级安全

---

## 2. 与 MimiSpec 的关系

| 语言 | 后缀 | 职责 | 特性 |
|------|------|------|------|
| **MimiSpec** | `.mms` | 渐进开发 | `$`/`?`、`desc`/`rule`、`steps:`、`math:` |
| **Mimi** | `.mimi` | 生产编译 | LLVM codegen、`requires`/`ensures`、`cap`、`parasteps` |

Mimi 文件通过 `mms {}` 超级注释块保留与 MimiSpec 设计的追溯链接：

```mimi
func pay(order: Order, amount: f64) -> Result<(), Err> {
    mms {
        func Pay(order, amount):
            desc "处理支付：检查余额、扣款、改状态"
            rule "支付必须幂等"
            requires: order.status == Pending
            ensures: order.status == Paid
            steps:
                check balance
                charge payment
                order.status = Paid to done
    }

    // Mimi 实现
    requires: order.status == Pending
    ensures: order.status == Paid

    let balance = check_balance(order)?;
    charge_payment(amount)?;
    order.status = Paid;
    Ok(())
}
```

`mms {}` 块是元数据，编译器忽略其内容。契约从 `mms {}` 块中提取作为设计参考，实现层可省略重复。

---

## 3. 类型系统

### 3.1 基础类型

| 类型类别 | 关键字 | 说明 |
|:---|:---|:---|
| 单元 | `unit` | 空元组 `()` 的类型 |
| 字符串 | `string` | UTF-8 编码，不可变 |
| 切片 | `&[T]` | 对连续内存的引用视图（规划中，当前用 `List<T>` 替代） |
| 可选 | `Option<T>` | 显式可空，**`null` 不是任意类型的子类型** |
| 无返回 | `nothing` | 表示不可达 |

### 3.2 ADT、模式匹配与 Newtype

```mimi
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
    Triangle { a: f64, b: f64, c: f64 }
}

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

透明别名使用 `type A = B`；需要强类型隔离时使用 `newtype`：

```mimi
type Meter = f64;        // 透明别名
newtype UserId = u64;    // 强类型 Newtype
```

支持 `trait`/`impl` 基础多态、`where` 约束语法、泛型函数与类型。trait 系统支持方法签名约定与静态分派（name mangling）。

---

## 4. 内存模型

Mimi 提供分层内存策略，遵循"默认最简单，复杂按需取"原则。

```
需要值的所有权吗？
├─ 唯一拥有者，无需共享
│   ├─ 值小且 Copy? → 自动 Copy
│   └─ 非 Copy → Move + 借用 (& / &mut)
├─ 需要多个拥有者
│   ├─ 跨线程共享 → shared T (原子 ARC)，可搭配 weak T
│   └─ 仅单线程 → local_shared T (Rc)
├─ 大量临时对象，生命周期限定在作用域内
│   └─ arena { ... } 内使用 ref T（严禁逃逸）
└─ 需要静态权限控制
    └─ 将 cap 作为函数参数线性传递，用后 drop
```

### 4.1 Move、借用与生命周期

- 非 `Copy` 类型默认 `Move`；`let y = x;` 后 `x` 不可用。
- 不可变引用 `&T`，可变引用 `&mut T`，遵循独占规则。
- 编译器自动推断绝大多数生命周期；复杂场景可显式标注 `'a`。

### 4.2 共享所有权

```mimi
type Node {
    parent: weak Node,
    children: List<shared Node>
}

type AppState {
    mut_counter: Mutex<i32>
}
```

`shared` 默认原子 ARC；单线程结构图使用 `local_shared`。`shared` 对象默认只读，内部可变性需使用 `Mutex<T>` / `RwLock<T>` 等。

### 4.3 Arena 区域内存

```mimi
func handle_req(req: Request) -> Response {
    arena {
        let ref temp_graph = build_graph(req);

        // Error：ref 禁止逃逸到全局结构
        // global_cache.push(temp_graph);

        return Response.new(temp_graph.result_nodes.copy())
    }
}
```

`arena { ... }` 块内分配的 `ref T` 生命周期等于该块，块出口自动回收。禁止将 `ref` Move 出 Arena 或赋给外层变量。

### 4.4 线性能力 `cap`

`cap` 类型不可复制、不可隐式丢弃，必须在每个控制流路径上被显式消费或传递。

```mimi
cap FileReadCap;
cap FileWriteCap;

cap FullFileAccess = FileReadCap + FileWriteCap;

func write_config(path: string, data: string, cap: FileWriteCap) -> Result<(), Err> {
    std::fs::write(path, data)!;
    drop(cap);   // 显式消费
    Ok(())
}
```

能力组合使用 `+`，与借用 `&` 无歧义。组合后的能力可分解：

```mimi
func rw_task(full: FullFileAccess) -> Result<(), Err> {
    let (r, w) = full.split();
    // 分别使用 r、w，最后各自 drop
    drop(r);
    drop(w);
    Ok(())
}
```

---

## 5. 并发模型

### 5.1 `actor`

`actor` 内部状态只能由该 actor 自身的方法修改；方法调用是异步消息，返回 `Future<T>`，必须 `await`。

```mimi
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count += 1;
    }

    func get_count() -> i32 {
        return self.count;
    }
}

let my_counter = Counter.spawn();
await my_counter.increment();
let current = await my_counter.get_count();
```

### 5.2 `parasteps`

`parasteps` 表示时间上的并行步骤块。内部 `spawn` 产生 `Future`，块结尾隐式 `await` 全部子任务；任一失败则取消其余任务。

```mimi
func load_dashboard(user_id: u64) -> Result<Dashboard, Err> {
    let (profile, orders) = parasteps "同时请求数据" {
        let p = spawn fetch_profile(user_id);
        let o = spawn fetch_orders(user_id);
        await (p, o)
    }!;

    return Ok(Dashboard(profile, orders));
}
```

### 5.3 结构化 Saga 补偿：`on failure`

`on failure { ... }` 注册到当前作用域的补偿栈，退出作用域时若未触发失败则丢弃，若触发则按 LIFO 逆序执行。

```mimi
func booking() -> Result<(), Err> {
    let seat = reserve_seat()?;
    on failure { cancel_seat(seat); }

    let hotel = book_hotel()?;
    on failure { cancel_hotel(hotel); }

    let payment = charge()?;
    on failure { refund(payment); }

    Ok(())
}
```

---

## 6. 契约、数学块与规则

### 6.1 函数契约前缀

```mimi
func withdraw(mut account: Account, amount: Money) -> Result<(), Err> {
    requires: account.balance >= amount
    ensures: account.balance == old(account.balance) - amount

    account.balance -= amount;
    Ok(())
}
```

- `requires`：前置条件；
- `ensures`：后置条件，可用 `old(x)` 引用入口值。

`old()` 对非 `Copy` 类型会触发隐式 `copy()`，编译器**强制发出 Warning**，开发者可自行管理快照以避免拷贝。

### 6.2 契约验证模式

| 模式 | 行为 |
|------|------|
| `mimi build`（默认） | 运行时断言：requires/ensures 在进入/退出函数时检查 |
| `mimi build --verify-contracts` | 使用 Z3 SMT 求解器证明合约（部分支持） |

### 6.3 编译期元编程

v1.0 对 `comptime` 进行裁剪，仅保留最小可用的卫生代码生成能力。

- 支持 `quote! { ... }` 声明宏级别的 AST 生成；
- `comptime` 代码**严禁 I/O、网络、随机数、当前时间**等非确定性操作；
- 若需在编译期读取文件，使用内置宏 `comptime_read_file!("path")`。

```mimi
comptime func make_const(name: string, value: i32) -> AST {
    quote! {
        const $(name): i32 = $(value);
    }
}
```

---

## 7. 包管理与模块系统

- 项目根通过 `mimi.toml` 定义包名、版本、依赖；
- `src/` 下每个 `.mimi` 文件自动成为一个模块；
- 默认私有，使用 `pub` 导出；
- 导入使用 `use`，路径分隔 `::`，字段访问 `.`；

```mimi
use std::collections::Map;
use crate::models::User;
use super::helper;
use another_package::some_func;
```

---

## 8. 版本说明

Mimi 的版本号不表示与 v1.0 的距离。v0.8 → v0.9 后可以是 v0.10、v0.11……每个版本迭代交付有限的特性或修复，版本号持续递增。

**v1.0 是定性里程碑**，不是"v0.x 之后的下一个号码"。当语言功能稳定、生产就绪声明条件满足时，才会标记 v1.0。

---

## 9. 版本历史

- v0.x - 早期草案：定义核心语法、内存模型、并发、Saga 补偿。
- v1.0 - 基线整合版：确立 `cap` 显式 drop + `+` 组合、`newtype` / `type` 别名分工、契约检查分级等基线决策。
- v1.1 - 特性扩展版：新增 `cap.split()` 能力分解、`old()` 契约快照语义、`math:` 块、trait 动态分派（vtable）、`extern "C"` FFI 块支持。
- v1.2 - 集成版：新增 `mms {}` 超级注释支持 MimiSpec 嵌入，实现意图→实现的契约绑定。
- v1.3 - 工具链版（规划中）：格式化器、静态分析器、LSP 增强。

---

## 10. Mimi v1.0 特性表

| 特性 | v1.0 决策 | 说明 |
|------|----------|------|
| 基本类型、函数、闭包 | ✅ 保留 | 核心基础 |
| Move / 借用 / 生命周期 | ✅ 保留 | 生命周期基本推断，极少显式标注 |
| `shared` / `local_shared` / `weak` | ✅ 保留 | 统一多所有权 |
| Arena + `ref` | ✅ 保留 | 区域内存一等语法 |
| `cap` 线性能力 | ✅ 保留 | 显式 `drop`，组合语法 `+` |
| `actor` | ✅ 保留 | 轻量任务调度，异步消息 |
| `parasteps` + `on failure` | ✅ 保留 | 结构化并发与 Saga 补偿 |
| `requires` / `ensures` | ✅ 保留 | 运行时断言；`--verify-contracts` Z3 验证 |
| `newtype` | ✅ 新增 | 强类型隔离包装 |
| `type A = B` 别名 | ✅ 保留 | 透明类型别名 |
| `use` | ✅ 兼容 | 模块导入 |
| `drop` 显式丢弃 | ✅ 保留 | 与所有权系统一致 |
| ADT + `match` 穷尽性 | ✅ 保留 | 现代语言标配 |
| `comptime` 元编程 | ⚠️ 裁剪 | 仅支持 `quote!` 宏 |
| `trait` / `impl` | ✅ 保留 | 基础多态，静态分派 |
| 类型泛型（带约束） | ✅ 保留 | `where T: Trait` 约束语法 |
| `mms {}` 块 | ✅ 保留 | MimiSpec 意图→实现追溯链接 |
| 编译到原生代码 | ✅ 保留 | LLVM codegen 后端，`mimi build` 生成可执行文件 |
| 自定义分配器 | ❌ 推迟 | 超出 1.0 范围 |
