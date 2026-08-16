# 04 - Mimi 并发模型

---

## 1. 总览

Mimi 提供两种并发模型：

| 模型 | 关键字 | 适用场景 |
|------|--------|----------|
| **Actor** | `actor`, `.spawn()`, `await` | 有状态并发实体 |
| **Parasteps** | `parasteps`, `spawn`, `await` | 无状态并行计算 |

两者都基于轻量级任务（非 OS 线程），由运行时调度。

---

## 2. Actor

### 2.1 定义 Actor

```mimi
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count += 1;
    }

    func get_count() -> i32 {
        self.count
    }

    func reset() {
        self.count = 0;
    }
}
```

### 2.2 使用 Actor

直接方法调用（同步，在内部线程执行并等待结果）：

```mimi
func main() -> i32 {
    let counter = Counter.spawn();

    // 直接调用：同步执行
    counter.increment();
    counter.increment();
    counter.increment();

    let count = counter.get_count();
    println(count);  // 3

    0
}
```

使用 `spawn` + `await`（异步，返回 Future 后等待）：

```mimi
func main() -> i32 {
    let counter = Counter.spawn();

    // spawn 创建 Future，await 等待结果
    let future = spawn counter.increment();
    await future;

    let count = await spawn counter.get_count();
    println(count);

    0
}
```

### 2.3 Actor 的性质

- **状态封装**：内部状态只能通过方法访问
- **消息 FIFO**：同一 actor 的消息按顺序处理
- **无全局顺序**：不同 actor 间无顺序保证
- **轻量级**：不占用 OS 线程，由运行时调度
- **同步 / 异步均可**：直接调用为同步，`spawn` + `await` 为异步

### 2.4 Actor 示例：银行账户

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

    func get_balance() -> f64 {
        self.balance
    }
}

func main() -> i32 {
    let account = BankAccount.spawn();

    account.deposit(100.0);
    let cash = account.withdraw(30.0)?;
    let balance = account.get_balance();

    println("Balance: ", balance);  // 70.0
    0
}
```

### 2.5 Actor 示例：聊天室

```mimi
actor ChatRoom {
    mut messages: List<string> = [];

    func send(msg: string) {
        push(self.messages, msg);
    }

    func get_messages() -> List<string> {
        self.messages
    }

    func clear() {
        self.messages = [];
    }
}

func main() -> i32 {
    let room = ChatRoom.spawn();

    room.send("Hello!");
    room.send("How are you?");

    let msgs = room.get_messages();
    println(msgs);

    0
}
```

---

## 3. Parasteps

### 3.1 基本用法

```mimi
func load_data() -> (string, string) {
    parasteps {
        let a = spawn fetch("api/users");
        let b = spawn fetch("api/orders");
        await (a, b)
    }
}
```

### 3.2 并行执行

`parasteps` 内部的 `spawn` 并发执行，块结尾隐式 await 所有子任务：

```mimi
func process() -> i32 {
    parasteps {
        let a = spawn slow_computation(1);
        let b = spawn slow_computation(2);
        let c = spawn slow_computation(3);

        // 隐式 await a, b, c
        // 返回最后一个 spawn 的结果
    }
}
```

### 3.3 结构化并发

parasteps 提供结构化并发保证：

- 所有 spawn 子任务在块退出前 join（隐式 await）
- 非 spawn 语句顺序执行
- **无失败取消语义**（v1.0）：`on failure` 按 0.34.17 Fault 范围收缩独立执行，但任一子任务失败**不会**取消其余子任务——它们正常 join 完成

> 0.34.23 §12 决策：README 旧版宣称的"任一失败取消其余子任务"未实现且不承诺，
> v1.0 移除。interp（`mimi run`）当前顺序执行 spawn（降级求值，结果等价）；
> codegen（`mimi build`）对 spawn 起真线程。并行化确定性语义 post-1.0 评估。

```mimi
func process() -> Result<i32, string> {
    parasteps {
        let a = spawn risky_op1();
        let b = spawn risky_op2();

        // 如果 a 失败：
        // 1. b 被取消
        // 2. a 的 on failure 执行
        // 3. b 的 on failure 执行
        // 4. parasteps 整体返回错误

        let r1 = await a;
        let r2 = await b;
        r1 + r2
    }!
}
```

### 3.4 带标签的 parasteps

```mimi
parasteps "同时加载用户数据" {
    let profile = spawn fetch_profile(user_id);
    let orders = spawn fetch_orders(user_id);
    await (profile, orders)
}
```

> **注意**：带字符串标签的形态是规划语法，v1.0 parser 尚不支持（只接受
> `parasteps { ... }`）。0.34.23 §12 决策：标签语法不进入 1.0，保持无标签
> 形态。

---

## 4. Spawn 与 Await

### 4.1 spawn

`spawn` 创建一个并发任务，返回 Future：

```mimi
let future = spawn async_func(args);
```

> **实现说明（0.37.71+）**：`spawn` 默认提交到有界 Worker Pool，复用线程而非
> 每个 Future 新建一个 OS 线程。若某个池内任务内部再次 `spawn`，子任务会自动
> 落到专用线程，避免所有 worker 都在等待子任务时产生池内死锁。
>
> **嵌套线程上下文（0.37.101+）**：专用 nested-spawn 线程同样被视为 worker
> 上下文；从其中再次 spawn 仍走专用线程。因此深层链式嵌套与二叉 fan-out
> 都不会重新排入有界 pool 导致死锁（`stress_native_nested_spawn_*` 覆盖）。

### 4.2 await

`await` 等待 Future 完成：

```mimi
let result = await future;
```

### 4.3 并行执行多个 spawn

```mimi
let a = spawn task1();
let b = spawn task2();
let c = spawn task3();

let r1 = await a;
let r2 = await b;
let r3 = await c;
```

或使用 parasteps：

```mimi
parasteps {
    let a = spawn task1();
    let b = spawn task2();
    let c = spawn task3();
    await (a, b, c)
}
```

## 5. 并发原语

Mimi 内置一组轻量并发原语。这些原语以 i64 handle 为运行时表示，但在类型系统中
使用名义类型（`AtomicI32`、`AtomicI64`、`AtomicBool`、`Mutex<T>`、`Channel<T>`）。

### 5.1 Channel

- `channel_new()` -> `Channel<T>`：创建有界消息通道
- `channel_send(ch, value)`：向通道发送一个 i64
- `channel_recv(ch)` -> `T`：阻塞接收
- `channel_try_recv(ch)` -> `i64`：非阻塞接收；空通道返回 `-1`
- `channel_drop(ch)`：释放通道

```mimi
func main() -> i32 {
    let ch = channel_new()
    channel_send(ch, 7)
    let v = channel_recv(ch)
    let empty = channel_try_recv(ch)   // -1
    channel_drop(ch)
    if v == 7 && empty == -1 { 0 } else { 1 }
}
```

### 5.2 Mutex

- `mutex_new(initial)` -> `Mutex<T>`：创建互斥锁并保存初始 i64
- `mutex_lock(m)` -> guard：加锁
- `mutex_get(guard)` -> `T`：读取受保护值
- `mutex_set(guard, value)`：写入受保护值
- `mutex_unlock(guard)`：解锁
- `mutex_drop(m)`：释放互斥锁

```mimi
func main() -> i32 {
    let m = mutex_new(0)
    let g = mutex_lock(m)
    mutex_set(g, 42)
    let v = mutex_get(g)
    mutex_unlock(g)
    mutex_drop(m)
    if v == 42 { 0 } else { 1 }
}
```

### 5.3 Atomic

- `AtomicI32`：`atomic_i32_new / load / store / fetch_add / compare_exchange / drop`
- `AtomicI64`：`atomic_i64_new / load / store / fetch_add / compare_exchange / drop`
- `AtomicBool`：`atomic_bool_new / load / store / compare_exchange / drop`

```mimi
func main() -> i32 {
    let a = atomic_i32_new(5)
    let old = atomic_i32_fetch_add(a, 3)
    let v = atomic_i32_load(a)
    atomic_i32_drop(a)
    if old == 5 && v == 8 { 0 } else { 1 }
}
```

---

## 6. On Failure 补偿

### 6.1 基本用法

`on failure` 注册补偿操作，在失败时按 LIFO 逆序执行：

```mimi
func booking() -> Result<(), string> {
    let seat = reserve_seat()?;
    on failure { cancel_seat(seat); }

    let hotel = book_hotel()?;
    on failure { cancel_hotel(hotel); }

    let payment = charge()?;
    on failure { refund(payment); }

    Ok(())
}
```

如果 `charge()` 失败：
1. `refund(payment)` 执行
2. `cancel_hotel(hotel)` 执行
3. `cancel_seat(seat)` 执行
4. 错误向上传播

### 6.2 补偿栈

补偿块注册到当前作用域的补偿栈：

```mimi
func process() -> Result<(), string> {
    let a = step1()?;
    on failure { undo_step1(a); }

    {
        let b = step2()?;
        on failure { undo_step2(b); }

        let c = step3()?;
        on failure { undo_step3(c); }
        // 内层作用域退出时，undo_step3 和 undo_step2 逆序执行
    }

    // 外层作用域
    Ok(())
}
```

### 6.3 补偿失败处理

补偿块本身失败时，错误累积为 `CompositeError`：

```mimi
func process() -> Result<(), string> {
    let resource = acquire()?;
    on failure {
        // 如果这个补偿失败，错误会累积
        release(resource)?;
    }

    risky_operation()?;
    Ok(())
}
```

---

## 7. Parasteps + On Failure

### 7.1 并行补偿

parasteps 内部的 spawn 有独立的补偿栈：

```mimi
func process() -> Result<(), string> {
    parasteps {
        let a = spawn task1()?;
        on failure { cleanup1(); }  // task1 的补偿

        let b = spawn task2()?;
        on failure { cleanup2(); }  // task2 的补偿

        await (a, b)
    }!
    // 如果 task1 或 task2 失败：
    // 1. 各自的 on failure 独立执行
    // 2. parasteps 整体向外抛错
    // 3. 外层的 on failure 执行
}
```

### 7.2 完整示例：并行事务

```mimi
func parallel_booking() -> Result<(), string> {
    let (seat_result, hotel_result) = parasteps "并行预订" {
        let seat = spawn reserve_seat();
        let hotel = spawn book_hotel();
        await (seat, hotel)
    }!;

    let seat = seat_result?;
    let hotel = hotel_result?;

    on failure {
        cancel_seat(seat);
        cancel_hotel(hotel);
    }

    let payment = charge()?;
    on failure { refund(payment); }

    Ok(())
}
```

---

## 8. 共享状态与并发安全

### 8.1 shared 跨线程

```mimi
func main() -> i32 {
    let counter = shared 0;

    parasteps {
        let a = spawn {
            // 通过 shared 在线程间共享
            *counter += 1;
        };
        let b = spawn {
            *counter += 1;
        };
        await (a, b);
    };

    println(*counter);  // 2
    0
}
```

### 8.2 local_shared 限制

```mimi
// local_shared 不能跨 parasteps 边界
let data = local_shared [1, 2, 3];

// ❌ 编译错误：local_shared 不能跨 parasteps
// parasteps {
//     let a = spawn {
//         let local = data.clone();  // 错误
//     };
// }

// ✅ 使用 shared
let data = shared [1, 2, 3];
parasteps {
    let a = spawn {
        let shared_clone = data.clone();  // OK
    };
}
```

---

## 9. 并发模式示例

### 9.1 并行数据处理

```mimi
func process_data(data: List<i32>) -> List<i32> {
    parasteps {
        let results = [];
        for item in data {
            push(results, spawn heavy_computation(item));
        }
        // 隐式 await 所有
        results
    }
}
```

### 9.2 超时控制

```mimi
func fetch_with_timeout(url: string) -> Result<string, string> {
    parasteps timeout 5s {
        let result = spawn fetch(url);
        await result
    }!
}
```

### 9.3 重试逻辑

```mimi
func retry<T>(f: func() -> T, max_retries: i32) -> T {
    let mut last_error = None;
    for i in range(0, max_retries) {
        match f() {
            Ok(v) => return v,
            Err(e) => last_error = Some(e)
        }
    }
    error(last_error.unwrap())
}
```
