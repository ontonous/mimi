# MimiSpec × Mimi 集成设计规范

> 本文档定义 MimiSpec (`.mms`) 与 Mimi (`.mimi`) 的协作模型、语法集成方案和 AI 协作工作流。
>
> **核心设计决策**：采用 `mms {}` 超级注释方案，MimiSpec 作为元数据嵌入 Mimi 代码，两语言保持独立语法，通过契约绑定实现形式化协作。

---

## 1. 设计背景

### 1.1 两语言的定位

| 语言 | 文件后缀 | 核心职责 | AI 角色 |
|------|----------|----------|---------|
| **MimiSpec** | `.mms` | 意图描述、规则约束、AI 协作 | AI 是**主要实现者** |
| **Mimi** | `.mimi` | 生产实现、内存安全、性能 | AI 是**协作生成者** |

### 1.2 设计目标

1. **保持独立性**：两语言各自演进，不强制语法兼容
2. **实现耦合**：通过 `mms {}` 块实现意图→实现的绑定
3. **AI 协作**：AI 清晰区分"要什么"和"怎么做"
4. **编译器验证**：编译器验证实现是否满足意图

---

## 2. 核心设计：`mms {}` 超级注释

### 2.1 语法定义

```mimi
func pay(order: Order, amount: f64) -> Result<(), Err> {
    mms {
        // MimiSpec 意图（缩进语法）
        func Pay(order, amount):
            desc "处理支付：检查余额、扣款、改状态"
            rule "支付必须幂等"
            requires: order.status == Pending and amount > 0
            ensures: order.status == Paid or order.status == Pending
            steps:
                check balance desc "检查余额"
                if insufficient:
                    error "余额不足" to exit
                charge payment desc "调用支付网关"
                on failure:
                    refund desc "补偿退款"
                order.status = Paid to done
    }
    
    // Mimi 实现（花括号语法）
    requires: order.status == Pending
    ensures: order.status == Paid
    
    let balance = check_balance(order)?;
    if balance < amount {
        return Err("余额不足".into());
    }
    charge_payment(amount).map_err(|e| {
        refund(amount);
        e
    })?;
    order.status = Paid;
    Ok(())
}
```

### 2.2 设计约束

| 约束 | 规则 | 理由 |
|------|------|------|
| **`mms {}` 是元数据** | 编译器忽略 `mms {}` 块内容 | 保持 Mimi 的独立编译能力 |
| **`mms {}` 内部是 MimiSpec** | 块内使用 MimiSpec 缩进语法 | 保持 MimiSpec 的核心价值 |
| **`mms {}` 不可嵌套** | `mms {}` 内部不能再有 `mms {}` | 避免递归复杂度 |
| **`mms {}` 位置自由** | 可以出现在函数体、类型定义、模块体中 | 灵活性 |
| **契约从 `mms {}` 提取** | `requires`/`ensures` 从 `mms {}` 块提取 | 避免重复表达 |
| **实现层可省略契约** | Mimi 代码中可以省略 `requires`/`ensures` | 编译器从 `mms {}` 块提取 |

### 2.3 使用场景

#### 场景 1：函数意图嵌入

```mimi
func process_order(order: Order) -> Result<(), Err> {
    mms {
        func ProcessOrder(order):
            desc "处理订单：验证库存、扣款、发货"
            rule "库存不能为负"
            requires: order.status == New
            ensures: order.status in [Paid, Cancelled]
            steps:
                check inventory
                if stock < order.qty:
                    error "库存不足" to exit
                charge payment
                on failure:
                    restore inventory
                order.status = Paid to done
    }
    
    // Mimi 实现
}
```

#### 场景 2：类型意图嵌入

```mimi
type Order {
    mms {
        type Order:
            desc "订单数据"
            id: u64
            status: OrderStatus
            amount: f64
    }
    
    id: u64,
    status: OrderStatus,
    amount: f64
}
```

#### 场景 3：模块意图嵌入

```mimi
module Shop {
    mms {
        module Shop:
            desc "订单管理模块"
            rule "所有操作必须有日志"
            
            type Order: ...
            func Pay: ...
            func Refund: ...
    }
    
    // Mimi 实现
}
```

#### 场景 4：状态机意图嵌入

```mimi
type OrderStatus {
    New,
    Pending,
    Paid,
    Shipped,
    Cancelled
}

mms {
    flow OrderLifecycle:
        New to Pending: desc "客户提交"
        Pending:
            to Paid: desc "支付成功"
            to Cancelled: desc "客户取消"
        Paid to Shipped: desc "已发货"
        Shipped to Delivered: desc "已送达"
}

// Mimi 实现：状态机逻辑
func process_order(order: Order) -> Result<(), Err> {
    match order.status {
        OrderStatus::New => {
            order.status = OrderStatus::Pending;
        }
        OrderStatus::Pending => {
            // 支付逻辑
        }
        // ...
    }
}
```

#### 场景 5：UI 意图嵌入

```mimi
type Order {
    id: u64,
    status: OrderStatus,
    amount: f64
}

mms {
    ui OrderPanel binds order:
        stack "订单面板":
            "订单 #order.id" desc "标题"
            parallel "操作栏":
                "支付" desc "按钮" on tap: process_payment(order)
                "取消" desc "按钮" on tap: cancel_order(order)
}

// Mimi 不实现 UI，但 AI 可以从 mms {} 块生成 React/SwiftUI 代码
```

---

## 3. 与现有语法的兼容性

### 3.1 完全兼容的语法

| 语法 | 兼容性 | 说明 |
|------|--------|------|
| `desc "..."` | ✅ 完全兼容 | 作为 MimiSpec 的核心构造 |
| `rule "..."` | ✅ 完全兼容 | 作为 MimiSpec 的核心构造 |
| `requires:` / `ensures:` | ✅ 完全兼容 | 从 `mms {}` 块提取 |
| `math:` | ✅ 完全兼容 | 作为 MimiSpec 的数学约束 |
| `steps:` | ✅ 完全兼容 | 作为 MimiSpec 的意图骨架 |
| `flow` | ✅ 完全兼容 | 作为 MimiSpec 的状态机描述 |
| `ui` | ✅ 完全兼容 | 作为 MimiSpec 的 UI 骨架 |
| `$`/`$$`/`?`/`??` | ✅ 完全兼容 | 可以加在 `mms {}` 块或 Mimi 代码上 |

### 3.2 需要调整的语法

| 语法 | 调整 | 理由 |
|------|------|------|
| `steps {}` | 新增块语法 | 原有 `steps:` 是实现体，新增 `steps {}` 是意图骨架 |
| `flow {}` | 新增块语法 | 原无对应，新增 `flow {}` 块 |
| `ui {}` | 新增块语法 | 原无对应，新增 `ui {}` 块 |

### 3.3 向后兼容性

- **现有 MimiSpec 文件**：✅ 完全兼容，可以直接放入 `mms {}` 块
- **现有 Mimi 文件**：✅ 完全兼容，不需要修改（`mms {}` 是新增语法）

---

## 4. AI 协作工作流

### 4.1 完整工作流

```
┌─────────────────────────────────────────────────────────────┐
│                      阶段 1：意图草图                        │
│  人类在 .mms 文件中写意图                                    │
│  AI 补全 steps/flow/ui                                      │
│  人类审查，逐步锁定 $/$$                                    │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                      阶段 2：意图嵌入                        │
│  人类将 .mms 内容放入 .mimi 文件的 mms {} 块                │
│  或 AI 自动将 .mms 转换为 .mimi + mms {} 块                 │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                      阶段 3：实现生成                        │
│  AI 读取 mms {} 块中的意图                                  │
│  AI 生成 Mimi 实现代码                                      │
│  人类审查，锁定 $/$$                                        │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                      阶段 4：契约验证                        │
│  编译器提取 mms {} 块中的契约                                │
│  编译器验证实现是否满足契约                                  │
│  输出验证报告                                                │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                      阶段 5：持续协作                        │
│  人类修改 mms {} 块中的意图                                  │
│  AI 更新 Mimi 实现                                          │
│  编译器重新验证                                              │
│  人类审查变更                                                │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 AI 读取意图

AI 可以清晰区分意图和实现：

```mimi
func pay(order: Order, amount: f64) -> Result<(), Err> {
    // AI 读取这些意图：
    mms {
        func Pay(order, amount):
            desc "处理支付：检查余额、扣款、改状态"  // ← AI 知道要做什么
            rule "支付必须幂等"                      // ← AI 知道约束
            requires: order.status == Pending         // ← AI 知道前置条件
            ensures: order.status == Paid             // ← AI 知道后置条件
            steps:                                   // ← AI 读取实现骨架
                check balance
                if insufficient:
                    error "余额不足"
                charge payment
                on failure:
                    refund
                order.status = Paid
    }
    
    // AI 根据意图和骨架生成实现
}
```

### 4.3 AI 生成实现

AI 读取意图层和 `steps {}` 骨架后，生成：

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
                if insufficient:
                    error "余额不足"
                charge payment
                on failure:
                    refund
                order.status = Paid
    }
    
    // AI 生成的实现：
    let balance = check_balance(order)?;
    if balance < amount {
        return Err("余额不足".into());
    }
    charge_payment(amount).map_err(|e| {
        refund(amount);
        e
    })?;
    order.status = Paid;
    Ok(())
}
```

### 4.4 人类审查锁定

人类审查 AI 生成的实现，逐步锁定：

```mimi
func$$ pay(order: Order, amount: f64) -> Result<(), Err> {
    // $$ 锁定：AI 不得修改此函数
    mms {
        func$$ Pay(order, amount):
            desc "处理支付：检查余额、扣款、改状态"
            rule "支付必须幂等"
            requires: order.status == Pending
            ensures: order.status == Paid
            steps:
                check balance
                if insufficient:
                    error "余额不足"
                charge payment
                on failure:
                    refund
                order.status = Paid
    }
    
    // 人类确认的实现
    let balance = check_balance(order)?;
    if balance < amount {
        return Err("余额不足".into());
    }
    charge_payment(amount).map_err(|e| {
        refund(amount);
        e
    })?;
    order.status = Paid;
    Ok(())
}
```

---

## 5. 编译器处理策略

### 5.1 `mms {}` 块处理

```rust
// 编译器伪代码
fn compile_function(func: FuncDef) {
    for stmt in func.body {
        match stmt {
            Stmt::MmsBlock { block } => {
                // mms {} 块：存储为元数据，不编译
                store_metadata(func.name, block);
                
                // 提取契约（如果启用 --verify-contracts）
                if verify_contracts {
                    let contracts = extract_contracts(block);
                    verify_contracts(func, contracts);
                }
            }
            _ => {
                // 其他语句：编译
                compile_stmt(stmt);
            }
        }
    }
}
```

### 5.2 契约提取

```rust
fn extract_contracts(mms_block: &MmsBlock) -> Contracts {
    let mut contracts = Contracts::new();
    
    for item in mms_block.items {
        match item {
            MmsItem::Requires(expr) => {
                contracts.requires.push(expr);
            }
            MmsItem::Ensures(expr) => {
                contracts.ensures.push(expr);
            }
            MmsItem::Rule(desc) => {
                contracts.rules.push(desc);
            }
            MmsItem::With(cap) => {
                contracts.caps.push(cap);
            }
            _ => {}
        }
    }
    
    contracts
}
```

### 5.3 契约验证

```rust
fn verify_contracts(func: &FuncDef, contracts: &Contracts) {
    // 1. 验证前置条件
    for requires in &contracts.requires {
        // 编译器验证调用者是否满足前置条件
        // 或在运行时插入断言
    }
    
    // 2. 验证后置条件
    for ensures in &contracts.ensures {
        // 编译器验证实现是否满足后置条件
        // 或在运行时插入断言
    }
    
    // 3. 验证权限传递
    for cap in &contracts.caps {
        // 编译器验证权限是否正确传递
    }
}
```

---

## 6. 与现有设计的整合

### 6.1 整合 MimiSpec 的核心价值

| MimiSpec 特性 | 整合方式 | 说明 |
|---------------|----------|------|
| `desc` | ✅ 保留在 `mms {}` 块中 | AI 协作的核心信号 |
| `rule` | ✅ 保留在 `mms {}` 块中 | 约束声明的核心机制 |
| `?`/`??` | ✅ 保留在 `mms {}` 块中 | 不确定性的显式表达 |
| `$`/`$$` | ✅ 保留在 `mms {}` 块中 | 锁定语义的核心 |
| `steps:` | ✅ 保留在 `mms {}` 块中 | 意图骨架的核心 |
| `flow` | ✅ 保留在 `mms {}` 块中 | 状态机意图的表达 |
| `ui` | ✅ 保留在 `mms {}` 块中 | UI 意图的表达 |
| 缩进语法 | ✅ 保留在 `mms {}` 块中 | 草图模式的核心 |

### 6.2 整合 Mimi 的核心价值

| Mimi 特性 | 整合方式 | 说明 |
|-----------|----------|------|
| 花括号体 | ✅ 保持独立 | 实现模式的核心 |
| `actor` | ✅ 保持独立 | 并发模型的核心 |
| `cap` | ✅ 保持独立 | 权限控制的核心 |
| `shared`/`local_shared` | ✅ 保持独立 | 内存模型的核心 |
| `trait`/`impl` | ✅ 保持独立 | 多态的核心 |
| `match` | ✅ 保持独立 | 模式匹配的核心 |
| `?` 操作符 | ✅ 保持独立 | 错误传播的核心 |

---

## 7. 与 Markdown 类比的精确性

| Markdown 特性 | MimiSpec 嵌入 | 一致性 |
|---------------|---------------|--------|
| ` ```python ` 代码块 | `mms {}` 意图块 | ✅ 一致 |
| 代码块内容透传 | 意图块内容存储为元数据 | ✅ 一致 |
| Markdown 解析器忽略代码块 | Mimi 编译器忽略 `mms {}` 块 | ✅ 一致 |
| 代码块内语法高亮 | `mms {}` 块内 MimiSpec 语法高亮 | ✅ 一致 |
| 多种语言代码块 | 多种意图块（steps/flow/ui） | ✅ 一致 |
| 代码块可嵌套 | `mms {}` 块不可嵌套 | ⚠️ 差异（可接受） |

---

## 8. 实施路径

### 阶段 1：语法支持（短期）

1. **Mimi 解析器**
   - 新增 `TokenKind::Mms` 关键字
   - 解析 `mms { ... }` 块
   - 存储为 `Stmt::MmsBlock` AST 节点

2. **MimiSpec 解析器**
   - 保持现有语法不变
   - 支持从 `mms {}` 块提取内容

### 阶段 2：契约提取（中期）

1. **契约提取器**
   - 从 `mms {}` 块提取 `requires`/`ensures`/`rule`
   - 生成契约文件（`.contract.mms`）

2. **契约验证器**
   - 验证 Mimi 实现是否满足契约
   - 输出验证报告

### 阶段 3：AI 工具链（长期）

1. **AI 读取工具**
   - AI 读取 `mms {}` 块中的意图
   - AI 读取锁定语义决定修改权限

2. **AI 生成工具**
   - AI 根据意图和骨架生成实现
   - AI 生成时保持契约一致

---

## 9. 总结

### 9.1 核心设计决策

| 决策 | 选择 | 理由 |
|------|------|------|
| **耦合方式** | `mms {}` 超级注释 | 优雅嵌入，保持独立 |
| **语法兼容** | 保持独立 | 各语言独立演进 |
| **契约绑定** | 从 `mms {}` 块提取 | 避免重复表达 |
| **AI 协作** | AI 读取意图，生成实现 | 清晰分工 |
| **编译器验证** | 验证契约满足性 | 形式化保证 |

### 9.2 优势

- ✅ MimiSpec 的缩进语法得以保留
- ✅ Mimi 的花括号语法得以保留
- ✅ 两语言通过 `mms {}` 块自然耦合
- ✅ AI 可以清晰区分意图和实现
- ✅ 编译器可以验证契约满足性
- ✅ 完全向后兼容

### 9.3 与现有设计的兼容性

- ✅ 现有 MimiSpec 文件可以直接放入 `mms {}` 块
- ✅ 现有 Mimi 文件不需要修改
- ✅ MimiSpec 的核心价值得以保留
- ✅ Mimi 的核心价值得以保留
