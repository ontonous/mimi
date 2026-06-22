# Actor 模型规范 (v1.0)

> 本文档定义 Mimi 语言中 Actor 的完整语义、生命周期和实现约束。
> 对应版本: v0.11 Actor 规范化

---

## 1. 核心概念

Mimi 的 Actor 是一种 **有状态并发实体**，满足：
- 状态封装：内部字段只能由该 actor 的方法访问
- 消息驱动：方法调用通过 **mailbox（消息队列）** 发送
- FIFO 顺序：同一 actor 的消息严格按发送顺序处理
- 透明执行：调用者不感知消息传递过程

### Actor vs Parasteps 对比

| 特性 | Actor | Parasteps |
|------|-------|-----------|
| 状态 | 有（封装在实体内部） | 无（纯函数式） |
| 通信 | 消息队列（mailbox） | Shared / 返回值 |
| 顺序 | FIFO 保证 | 无保证 |
| 生命周期 | 从 spawn() 到引用归零 | 块退出即结束 |

---

## 2. 定义 Actor

```mimi
actor Name {
    // 字段（默认不可变，mut 可写）
    field: Type = default_value;
    mut mutable_field: Type = default_value;

    // 方法（不可重载，不支持运算符）
    func method_name(param: Type) -> ReturnType {
        // body, 可通过 self. 访问字段
    }
}
```

示例：

```mimi
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get_count() -> i32 {
        self.count
    }
}
```

### 约束
- Actor 字段不能包含 `c_shared`/`c_borrow`/`c_borrow_mut` 类型（FFI 安全限制）
- Actor 方法不能是 `pub`（访问通过 actor 实例，不可外部直接调用）
- Actor 内部不支持 `parasteps` 嵌套（但允许 `spawn` + `await`）

---

## 3. Actor 生命周期

```
                ┌─────────────┐
     spawn() ──>│   Init      │── 创建 mailbox + 字段初始化
                └──────┬──────┘
                       │
                ┌──────▼──────┐
                │  Running    │── 从 mailbox 接收并处理消息
                └──────┬──────┘
                       │
                ┌──────▼──────┐
                │  Draining   │── mailbox 关闭，处理剩余消息
                └──────┬──────┘
                       │
                ┌──────▼──────┐
                │  Terminated │── worker 线程退出
                └─────────────┘
```

### 3.1 创建：`spawn()`

`ActorType.spawn()` 执行以下步骤：
1. 创建 mailbox（`mpsc::channel`）
2. 初始化所有字段为默认值
3. 启动 worker 线程（绑定到 mailbox 的 receiver）
4. 返回 `ActorHandle`（绑定到 mailbox 的 sender）

示例：
```mimi
let counter = Counter.spawn();  // 返回 ActorHandle
```

### 3.2 运行：消息处理

Worker 线程循环从 mailbox 取消息，按 FIFO 顺序处理每条消息：
1. 提取方法名和参数
2. 执行方法体（写字段、读字段）
3. 如有 `ResponseChannel`，将结果发回
4. 循环直到 mailbox 关闭且队列为空

### 3.3 终止

Actor 在以下条件满足时终止：
- 所有 `ActorHandle` 引用被丢弃 (drop)
- mailbox 的 sender 端全部关闭
- 剩余消息被 drain 完毕

---

## 4. 方法调用语义

### 4.1 直接调用（同步）

```mimi
counter.increment();        // 同步
let val = counter.get();    // 同步，返回 i32
```

**语义**：调用者发送消息到 mailbox，然后阻塞直到 worker 处理完毕并返回结果。
对于返回 `()` 的方法（如 `increment()`），调用者等待处理完成后继续。
对于返回值的方法（如 `get()`），调用者等待结果返回。

### 4.2 `await` 调用（异步等待结果）

```mimi
let result = await counter.get();  // 异步等待
```

**语义**：等价于直接调用。解析器处理 `await actor.method()` 语法，
最终调用 mailbox 模式发送消息并等待响应。

> 在当前实现中，`await counter.method()` 与 `counter.method()` 在 interpreter
> 中行为一致（都是通过 mailbox + 等待响应）。未来 `await` 可能用于非 actor
> Future 场景（如 spawn expr）。

### 4.3 `spawn` 调用（投递即忘）

```mimi
let future = spawn counter.increment();
await future;  // 等待执行完成
```

**语义**：将消息投递到 mailbox 后立即返回 `Future<T>`。调用者可在后续 `await`
中等待结果。这允许多个消息依次投递到同一 actor 的 mailbox，然后统一等待。

```mimi
let a = spawn counter.increment();
let b = spawn counter.increment();
await a;
await b;  // 两个 increment 在 mailbox 中顺序执行
```

### 4.4 FIFO 保证

所有发往同一 actor 的消息严格按发送顺序被 worker 处理：

```mimi
counter.increment();          // 消息 1 入队
counter.increment();          // 消息 2 入队
let val = counter.get();      // 消息 3 入队
// 处理顺序: 消息 1 → 消息 2 → 消息 3
// val == 2 (正确)
```

---

## 5. 错误处理

### 5.1 方法错误

如果 actor 方法返回 `Result::Err` 或使用 `?` 传播错误：

```mimi
actor Bank {
    mut balance: i32 = 0;
    func withdraw(amount: i32) -> Result<i32, string> {
        if self.balance >= amount {
            self.balance = self.balance - amount;
            Ok(amount)
        } else {
            Err("insufficient funds")
        }
    }
}
```

错误通过 ResponseChannel 返回给调用者。调用者得到 `Err(...)`。

### 5.2 Worker Panic

如果 actor worker 线程 panic：
1. mailbox 的 sender 端收到错误
2. 所有阻塞等待响应的调用者得到 `InterpError`（worker crashed）
3. 新消息被拒绝（mailbox closed）
4. Actor 进入 Terminated 状态

### 5.3 超时（未来）

未来可选的调用超时：
```mimi
// 未来语法
let result = timeout(5s) { counter.withdraw(100) };
```

---

## 6. 线程安全

### 6.1 单 Actor 内部
- mailbox 保证消息串行处理，无数据竞争
- 字段访问不需要锁（模型级保证）

### 6.2 跨 Actor
- 不同 Actor 的 worker 线程可以并行执行
- 消息顺序在不同 actor 间无全局保证

### 6.3 多线程调用者
- 多个线程可安全地调用同一 actor 的方法
- 消息通过 `mpsc::Sender` 发送（Send + Sync）
- 调用者线程不会同时持有 actor 的内部锁

---

## 7. 实现要点

### 7.1 数据结构

```
ActorMessage {
    method: String,
    args: Vec<Value>,
    response: Option<oneshot::Sender<Result<Value, InterpError>>>,
}

ActorInstance {
    actor_name: String,
    fields: HashMap<String, Value>,
    methods: Vec<FuncDef>,
    receiver: Mutex<mpsc::Receiver<ActorMessage>>,
}

ActorHandle {
    sender: mpsc::Sender<ActorMessage>,
    worker: Option<JoinHandle<()>>,
    id: usize,
}
```

### 7.2 字段访问

Worker 线程直接持有 `ActorInstance` 的独占所有权（不在 RwLock 中），
因为所有消息已经通过 mailbox 串行化。调用者通过 `ActorHandle.sender`
提交消息，worker 处理消息时修改字段、执行方法。

### 7.3 Codegen 注意事项

当前 codegen 路径：
- Actor 被编译为 LLVM struct（值传递）
- 方法通过 `self: *ActorName` 指针访问
- `spawn()` 构造函数分配堆内存 + 初始化字段
- mailbox 模式尚未在 codegen 中实现（v0.11 范围限制）

---

## 8. 迁移与兼容性

所有现有 actor 代码在 interpreter 下保持向后兼容。
现有测试（`actor_await_method`、`actor_sync_method_still_works` 等）
均无需修改。

DCU（Design Compatibility Unit）验证：
- `demo/13_actors.mimi` — 直接方法调用 ✅
- `examples/actor_full_test.mimi` — spawn + 方法调用 ✅
- 所有 `dual_actor_*` 测试 — 双后端等价 ✅
