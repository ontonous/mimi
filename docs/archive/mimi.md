# Mimi 语言 (.mimi) 核心设计规范 v1.0

Mimi 是面向 **Intent-as-Code + Safe AI Collaboration** 的系统编程语言。它的核心目标不是单纯的性能或表达力，而是把“人类已锁定的决策”与“AI 可生成的未确定区域”之间的边界，变成编译器、IDE 和构建工具可以直接执行的一等语言构造。

本规范是 Mimi v1.0 的基线设计，整合自 `mimi.md`、`mimigramma.md` 以及 `2.md` 中的最新决策。它同时明确 `.mms`（MimiSpec 草图模式）与 `.mimi`（Mimi 生产模式）的语法边界，以及 1.0 版本必须实现、推迟或移除的特性。

---

## 0. Mimi 的精确切入点：Intent-as-Code + Safe AI Collaboration

传统系统语言默认源码完全由人类编写，AI 只是外部辅助；Mimi 从语法层为“人与 AI 共同构造生产代码”设计基础设施：

- `?` / `??` 标记 AI 可生成、可修改的区域；
- `$` / `$$` 标记人类已锁定、AI 不得触碰的区域；
- `desc` / `rule` / `requires` / `ensures` 提供机器可读的意图上下文；
- `parasteps` / `on failure` 为 AI 生成的并发与事务代码提供结构化骨架；
- `cap` 让 AI 生成的代码无法隐式获得 I/O 等敏感权限。

因此，Mimi 的差异化定位可以概括为：

> **Intent-as-Code + Safe AI Collaboration**：Mimi 是一门将“人类锁定 / AI 生成”的边界作为一等语法构造的系统语言，使可信的人机协同软件构造成为编译流程的内在环节。

它不与 Rust 比拼极限裸性能，也不与 Python 比拼原型速度，而是在 **“AI 辅助软件构造的可信度”** 这一维度上建立差异。

---

## 1. 设计支柱

### 1.1 渐进光谱：从草图到生产的单一事实源

Mimi 的核心哲学是 **“代码不必须一次性写完，但在任何阶段都必须是合法的”**。同一个文件可以同时包含从 `...` 占位、自然语言 `desc`、结构化契约，到精确实现代码的连续形态，中间产物永不废弃。

| 层级 | 语法形式 | 语义 | 典型场景 |
| :--- | :--- | :--- | :--- |
| **L1 占位** | `...` | 此处有待填充，完全委托 | 函数体或某个分支待定 |
| **L2 意图描述** | `desc "..."` | 自然语言说明，意图明确但实现未定 | 步骤流占位、AI 指南 |
| **L3 逻辑蓝图** | `.mms` 契约关键字 | 结构化决策、契约（`requires`/`ensures`/`math`/`rule`） | 接口与规则已锁定，等待实现 |
| **L4 生产实现** | `.mimi` 具体代码 | 可编译执行的精确实现 | 生产就绪、性能调优、人工锁定（`$`/`$$`） |

`.mms` 是 `.mimi` 的草图模式，`.mimi` 是 `.mms` 的超集。提升（`mimi promote` 或直接改扩展名）后，同一文件仍是唯一事实源，无需在“意图文档”与“实现代码”之间做双向同步。

### 1.2 逻辑（相对）安全：从内存安全到契约安全

Mimi 的第二个核心支柱是 **逻辑（相对）安全**。Rust 把“内存安全”提升为系统语言的主流目标；Mimi 在此基础上进一步把“逻辑正确性 / 意图一致性”也变成一等构造。实现手段包括：

- **一等契约**：`requires` / `ensures` / `math:` 块直接写在函数签名附近，不是外部注解；
- **自然语言约束**：`rule` 作为可附着于任意实体的元数据，供人类、AI 与工具解读；
- **意图锁**：`$` / `$$` / `?` / `??` 把“谁对这段代码负责”编码进语法树；
- **结构化 Saga 补偿**：`on failure` 让事务性补偿与正常控制流共存；
- **线性能力**：`cap` 使 I/O、文件、网络等敏感权限必须显式传递，无法被 AI 生成的代码隐式获取。

v1.0 的契约检查以**静态简单检查**和**运行时断言**为主，完整的 SMT / AI 证明模式（`--verify-contracts`、`--verify-rules`）推迟到 v1.1。

### 1.3 零开销的实用系统抽象

Mimi 为常见系统编程场景提供默认安全、按需精确的策略：

- 值默认 `Move`，基本类型自动 `Copy`；
- 多所有权使用 `shared` / `local_shared` + `weak`；
- 区域内存使用 `arena { ... }` + `ref T`；
- 敏感权限使用 `cap` 线性传递；
- 并发使用 `actor` 与 `parasteps`。

---

## 2. `.mms` 与 `.mimi` 的语法边界

### 2.1 双模式

| 模式 | 扩展名 | 典型命令 | 行为 |
| :--- | :--- | :--- | :--- |
| **Sketch / 草图模式** | `.mms` | `mimi check` | 允许 `...`、`desc`、未实现的 `steps`；校验语法、锁状态、规则附着 |
| **Production / 生产模式** | `.mimi` | `mimi build` | 要求 `$`/`$$` 锁定的 Fragment 必须有具体实现；编译为可执行产物 |

### 2.2 语法兼容性规则

| 构造 | `.mms` 中的形式 | `.mimi` 中的接受形式 | 说明 |
|------|----------------|-------------------|------|
| 类型定义 | `type A: B \| C` 或缩进记录体 | 同左；另外支持 `type A { ... }` 花括号记录 / ADT | `type` 在两种模式下都合法 |
| 函数体 | 缩进体（`steps:` 等） | **L4 实现必须使用花括号体 `{...}`**；签名后可保留缩进的 `requires:`/`ensures:`/`math:`/`desc:` 契约前缀 | 同一函数主体不能同时出现缩进实现体与花括号实现体 |
| `module` 体 | 缩进体 | **L4 模块必须使用花括号体 `{...}`**；兼容 `.mms` 缩进体 | `module Shop { ... }` 表示进入 L4 精确实现 |
| `rule` | 前置附着于实体 | 行为一致 | 语义不变 |
| `desc` | 独立实体 | 独立实体，也可在花括号体内作为语句（无运行时代码） | 统一作为元数据 |
| `use` | 文件级指令 | 允许，内部转换为 `use` 链 | 推荐写 `use`，`use` 保留兼容 |

### 2.3 L4 花括号体示例

```mimi
module Shop {

    // L4：已进入生产实现，使用花括号体
    func$$ process_local_cache() {
        // 人工强锁定的精确实现
    }

    // L3 + L4 混合：契约前缀 + 花括号实现体
    func create_order(req: Request) -> Result<Order, Err> {
        requires: req.items.not_empty()
        ensures: result.status == OrderStatus.New

        desc "第一步：从请求头解析用户身份"
        let user = parse_user(req)!;

        desc "第二步：防刷风控校验"
        ... // 尚未实现，编译时 AI 可补全

        return Ok(Order.new(user, req.items));
    }
}
```

> `result` 指代函数的返回值；具体命名约定由实现定义。

---

## 3. 类型系统

### 3.1 基础类型

| 类型类别 | 关键字 | 说明 |
|:---|:---|:---|
| 单元 | `unit` | 空元组 `()` 的类型 |
| 任意 | `any` | 顶层类型，仅可在 `comptime` 中解构（`unsafe` 块规划中） |
| 字符串 | `string` | UTF-8 编码，不可变 |
| 切片 | `&[T]` | 对连续内存的引用视图（规划中，当前用 `List<T>` 替代） |
| 可选 | `T?` / `Option<T>` | 显式可空，**`null` 不是任意类型的子类型** |
| 无返回 | `nothing` | 表示不可达或 `error`/`raise` 的类型 |

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

v1.0 支持 `trait`/`impl` 基础多态、`where` 约束语法、泛型函数与类型。trait 系统支持方法签名约定与静态分派（name mangling），动态分派（vtable）规划为 v1.1+。

---

## 4. 内存模型

Mimi 提供分层内存策略，遵循“默认最简单，复杂按需取”原则。

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

能力组合使用 `+`，与借用 `&` 无歧义。组合后的能力可分解（示意）：

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

每个 `actor` 实例由运行时轻量任务调度，消息 FIFO，不同 actor 间无全局顺序。

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

补偿块本身失败时，错误累积为 `CompositeError`，原错误始终为主错误。

`parasteps` 内失败时，各 `spawn` 的局部 `on failure` 先独立执行，全部清理完毕后 `parasteps` 整体向外抛错，再触发外部补偿。

---

## 6. 契约、数学块与规则

### 6.1 函数契约前缀

L4 函数可以在签名后、花括号体内保留缩进的契约前缀：

```mimi
func withdraw(mut account: Account, amount: Money) -> Result<(), Err> {
    requires: account.balance >= amount
    ensures: account.balance == old(account.balance) - amount

    account.balance -= amount;
    Ok(())
}
```

- `requires`：前置条件；
- `ensures`：后置条件，可用 `old(x)` 引用入口值；
- `math:`：结构化数学表达式块。

`old()` 对非 `Copy` 类型会触发隐式 `copy()`，编译器**强制发出 Warning**，开发者可自行管理快照以避免拷贝。

### 6.2 `math:` 块

```mimi
func cross_attention(query, key, value) {
    math:
        d_k = dim(key, -1)
        scores = query @ key.T / sqrt(d_k)
        weights = softmax(scores, -1)
        context = weights @ value
    // ...
}
```

`math:` 负责可解析、可静态检查的精确数学意图；`desc` 负责自然语言说明，两者互补。

### 6.3 `rule` 与自然语言约束

`rule "..."` 是前置约束修饰符，附着于下一个实体。v1.0 中 `rule` 作为元数据保留，默认不阻断编译；AI 深度审计与 `--verify-rules` / `--strict` 模式推迟到 v1.1。

### 6.4 编译模式

| 模式 | 行为 |
|------|------|
| `mimi build`（默认） | 仅对可静态求值的简单 `requires`/`ensures` 做检查；`rule` 作为元数据 |
| `mimi build --verify-contracts` | 使用 SMT 求解器尝试证明契约（**v1.1**） |
| `mimi build --strict` | 所有未锁定的 `rule` 必须通过证明或 AI 审计（**v1.1**） |

---

## 7. 编译期元编程

v1.0 对 `comptime` 进行裁剪，仅保留最小可用的卫生代码生成能力，避免实现复杂度爆炸。

- 支持 `quote! { ... }` 声明宏级别的 AST 生成；
- v1.0 **不支持** `type_info` 反射与通用编译期反射 API；
- `comptime` 代码**严禁 I/O、网络、随机数、当前时间**等非确定性操作；
- 若需在编译期读取文件，使用内置宏 `comptime_read_file!("path")`，由构建系统追踪依赖。

```mimi
comptime func make_const(name: string, value: i32) -> AST {
    quote! {
        const $(name): i32 = $(value);
    }
}
```

---

## 8. 包管理与模块系统

- 项目根通过 `mimi.toml` / `mimi.yml` 定义包名、版本、依赖；
- `src/` 下每个 `.mimi` 文件自动成为一个模块；
- 默认私有，使用 `pub` 导出；
- 导入使用 `use`，路径分隔 `::`，字段访问 `.`；
- Mimi 使用 `use` 导入；`@import "file.mms"` 是 MimiSpec 的导入语法，Mimi 不直接处理。

```mimi
use std::collections::Map;
use crate::models::User;
use super::helper;
use another_package::some_func;
```

---

## 9. 从草图到生产的工作流

1. **草图阶段**：创建 `.mms` 文件，用 MimiSpec 子集快速描述领域、类型、流程、规则。
2. **评审阶段**：人类与 Meowthos 协作，逐步把 `?`/`??` 转为无后缀，再转为 `$`/`$$`。
3. **提升阶段**：`mimi promote file.mms`（或直接重命名）变为 `.mimi`，进入生产模式。
4. **生产阶段**：补充 L4 花括号实现；已锁定的 Fragment 不得被 AI 修改。
5. **交付阶段**：文件 100% 锁定，且所有锁定 Fragment 都有具体实现，可编译运行。

---

## 10. 已知问题与开放决策

以下问题已由 `2.md` 决策解决：

| 问题 | 决策 |
|------|------|
| `func` 体形态 | L4 使用花括号体；缩进体仅作为 `.mms` 兼容或契约前缀 |
| `type` vs `struct` | 保留 `type`；`type A = B` 为透明别名，`newtype` 为强隔离包装 |
| 模块路径分隔符 | `::` 表示模块路径，`.` 表示字段访问 |
| 导入语法 | 推荐 `use`；`use` 保留兼容 |
| `cap` 线性类型矛盾 | 必须显式 `drop`；组合符改为 `+` |
| `null` 子类型 | 移除；使用 `Option<T>` / `T?` |
| `old()` 深拷贝成本 | 非 `Copy` 类型触发强制 Warning |
| 表达式子集边界 | `.mms` 保持简单表达式；`.mimi` 开放完整表达式 |

仍然开放的长期问题：

- Actor 运行时的形式化语义（邮箱容量、背压、跨 actor 循环 await 死锁检测）；
- `desc` 与代码实现一致性检查工具；
- 统一 parser 同时处理 `.mms` 草图模式与 `.mimi` 生产模式；
- IDE / Subagent 对 `$`/`$$` 锁的强制执行；
- 形式化验证后端选型（Z3 等）；
- 分布式 actor、效应系统、LSP。

---

## 11. MimiSpec 集成

Mimi 通过 `mms {}` 块支持嵌入 MimiSpec 意图描述，实现意图→实现的契约绑定。

### 11.1 `mms {}` 超级注释

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

### 11.2 设计约束

- `mms {}` 是元数据块，编译器忽略其内容
- `mms {}` 内部保持 MimiSpec 缩进语法
- 契约从 `mms {}` 块提取，实现层可省略重复
- 两语言保持独立，通过 `mms {}` 块耦合

### 11.3 详细设计

完整设计规范见 [`mms-integration.md`](./mms-integration.md)。

---

## 12. 版本说明

Mimi 的版本号不表示与 v1.0 的距离。v0.8 → v0.9 后可以是 v0.10、v0.11……每个版本迭代交付有限的特性或修复，版本号持续递增。

**v1.0 是定性里程碑**，不是"v0.x 之后的下一个号码"。当语言功能稳定、生产就绪声明条件满足时，才会标记 v1.0。在此之前，所有开发都在 v0.x 区间内自由迭代。

---

## 13. 版本历史

- v0.x - 早期草案：定义核心语法、AAM 内存模型、并发、Saga 补偿。
- v1.0 - 基线整合版：确立 L4 花括号体、逻辑安全支柱、`cap` 显式 drop + `+` 组合、`newtype` / `type` 别名分工、契约检查分级等基线决策。
- v1.1 - 特性扩展版：新增 `cap.split()` 能力分解、`old()` 契约快照语义、`math:` 块编译时求值、trait 动态分派（vtable）、`extern "C"` FFI 块支持（已在 mimi v0.1.1 实现）、`trait`/`impl` 基础多态与 `where` 约束语法（已在 v0.7 实现）。
- v1.2 - 集成版：新增 `mms {}` 超级注释支持 MimiSpec 嵌入，实现意图→实现的契约绑定。
- v1.3 - 工具链版（规划中）：格式化器、静态分析器、LSP 增强（foldingRange、Warning 级别诊断）。

---

## 14. Mimi v1.0 特性表

| 特性 | v1.0 决策 | 说明 |
|------|----------|------|
| 基本类型、函数、闭包 | ✅ 保留 | 核心基础 |
| Move / 借用 / 生命周期 | ✅ 保留 | 生命周期基本推断，极少显式标注 |
| `shared` / `local_shared` / `weak` | ✅ 保留 | 统一多所有权 |
| Arena + `ref` | ✅ 保留 | 区域内存一等语法 |
| `cap` 线性能力 | ✅ 保留 | 显式 `drop`，组合语法 `+` |
| `actor` | ✅ 保留 | 轻量任务调度，异步消息 |
| `parasteps` + `on failure` | ✅ 保留 | 结构化并发与 Saga 补偿 |
| `requires` / `ensures` / `math:` | ✅ 保留 | 基础静态检查；`--verify-contracts` 推迟 v1.1 |
| `desc` / `rule` 元数据 | ✅ 保留 | `rule` AI 审计推迟 v1.1 |
| `newtype` | ✅ 新增 | 解决 `type =` 别名与强包装歧义 |
| `type A = B` 别名 | ✅ 保留 | 透明类型别名 |
| `use` | ✅ 兼容 | 推荐 `use` |
| `drop` 显式丢弃 | ✅ 保留 | 与所有权系统一致 |
| ADT + `match` 穷尽性 | ✅ 保留 | 现代语言标配 |
| `comptime` 元编程 | ⚠️ 裁剪 | 仅支持 `quote!` 宏 |
| `interface` / `trait` | ✅ 保留 | 基础 trait 系统，支持方法签名约定与静态分派 |
| 类型泛型（带约束） | ✅ 保留 | `where T: Trait` 约束语法 |
| 自定义分配器 | ❌ 推迟 | 超出 1.0 范围 |
| 编译到原生代码 | ✅ 保留 | LLVM codegen 后端，`mimi build` 生成可执行文件 |
