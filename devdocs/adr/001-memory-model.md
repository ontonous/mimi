# ADR 001: Mimi 语言内存模型

## 状态

已接受（v0.7），持续演进中（v0.15 规划路径敏感借用检查）

---

## 上下文

Mimi 是 MimiSpec 的生产编译后端，需要支持：

1. **结构化并发**（`spawn`/`await` / `parasteps`）：跨线程共享可变状态
2. **合约验证**（`requires`/`ensures`）：运行时断言 + Z3 静态验证
3. **双后端**（解释器 + LLVM codegen）：内存模型必须在两端语义等价
4. **渐进复杂度**：优先可用，再求安全

关键权衡：引用计数（RC）vs 借用检查器（borrow checker）vs 线性类型（linear types）。

---

## 决策

### 1. `shared` 变量使用引用计数（`Arc<RwLock<T>>`）

```
shared let x = 42;          // 堆分配，Arc<RwLock<T>>
let y = x;                  // 拷贝引用（retain），非移动
```

- **解释器**（`src/interp/eval/stmt.rs:413`）：`Value::Shared(Arc::new(RwLock::new(v)))`
- **Codegen**（`src/codegen/mod.rs:425-432`）：调用 `mimi_rc_alloc` → `mimi_rc_retain` → `mimi_rc_release`
- **C runtime**（`src/runtime/mimi_runtime.c:239-277, 390-449`）：`MimiRcHeader { strong, weak }`，`malloc` 分配

**理由**：`Arc<RwLock<T>>` 是跨线程共享可变状态的最简方案。借用检查器需要路径敏感分析（v0.15 计划），线性类型在处理并发共享时不够灵活。RC 在解释器端用 `Arc`，codegen 端用 C `atomic`/非 atomic 计数器，语义对齐。

### 2. `weak` 引用通过 `upgrade()` 升级

```
weak let w = x;             // 不增加 strong 计数
let opt = w.upgrade();      // 返回 Option<T*>
```

- **Codegen**（`src/codegen/expr/call/method.rs:22-69`）：调用 `mimi_rc_upgrade`（CAS 循环保证原子升级）
- **C runtime**（`src/runtime/mimi_runtime.c:440-449`）：`atomic_compare_exchange_weak` 循环
- **作用域自动释放**（`src/codegen/scope.rs:186-192`）：scope exit 时调用 `mimi_rc_weak_release`

**理由**：弱引用解决循环引用问题（如观察者模式、图结构）。`upgrade()` 返回 `Option` 而非直接指针，保证空安全。

### 3. `arena` 分配用作栈式局部区域

```
arena {
    let ref x = compute();   // 在 arena 中分配
}                            // arena 释放，ArenaRef 逃逸被拒绝
```

- **解释器**（`src/interp/eval/stmt.rs:205-230`）：`Arena { id, slots: Vec<Value> }`，嵌套深度跟踪
- **Codegen**（`src/codegen/block.rs:339-394`）：`@llvm.stacksave` / `@llvm.stackrestore`
- **逃逸检测**（`src/core/check_stmt.rs:512-524`）：E0306 错误，禁止 `arena` 内部的 ref 赋值给外部变量
- **`contains_arena_ref`**（`src/interp/value.rs:526-563`）：递归扫描复合类型

**理由**：arena 提供零开销的批量释放，适合临时计算和 hot loop。逃逸检测在类型检查期完成，无需运行时开销。

### 4. 局部变量默认栈分配

普通 `let` 变量在栈上分配，`shared` 变量在堆上通过 RC 管理。两者泾渭分明：

| 类别 | 分配位置 | 生命周期 | 线程安全 |
|------|---------|---------|---------|
| `let x = ...` | 栈 | 作用域结束 | 不跨线程 |
| `shared let x = ...` | 堆（RC） | 引用计数归零 | `Arc<RwLock<T>>` |
| `arena { let ref x = ... }` | arena 区域 | arena 块结束 | 不跨线程 |
| `weak let w = ...` | 堆（弱引用） | strong=0 自动失效 | 原子计数 |

### 5. 为什么不直接用 Rust 的借用检查器

- **复杂度**：Mimi 需要路径敏感分析（如 `let r = &x.field`）、子类型化和重借用（`src/tests/borrow_boundary.rs` 记录 6 个已知边界情况）
- **双后端对齐**：借用检查器的跨语言（codegen ↔ C runtime）语义映射比 RC 复杂得多
- **渐进路线**：v0.15 计划引入路径敏感借用检查（AGENTS.md §12），但当前 v0.7-v0.13 优先完成验证覆盖和生态扩展

### 6. 分配跟踪

Codegen 路径通过 `heap_allocs` 跟踪每次 RC 分配（`src/codegen/mod.rs:145`）：

```rust
heap_allocs: RefCell<Vec<Vec<PointerValue>>>,
```

每个 scope 结束时，遍历 `shared_release_vars` 调用 `mimi_rc_release`（`src/codegen/scope.rs:170-230`）。解释器端依赖 Rust 的 `Arc` 自动 drop，在 `Value::Shared` 的 `Drop` 实现中处理引用计数（`src/interp/value.rs`）。

### 7. 为什么不是纯线性类型

Mimi 支持 `cap` 线性能力（`src/core/checker.rs:17`，`src/core/check_stmt.rs:288,622`），但**不**推广到所有类型。纯线性类型在以下场景产生摩擦：

- 并发共享需要 `Arc` 或 `Rc` 适配器
- 图结构 / 循环引用需要弱引用
- 运行时合约验证需要保留副作用的灵活性

因此采用混合方案：默认线性能力 + `shared` RC 逃逸口。

---

## 后果

### 正面

1. **实现简单**：双后端语义对齐容易，解释器用 `Arc`，codegen 用 C atomic
2. **并发友好**：`Arc<RwLock<T>>` 天然支持 `spawn`/`parasteps` 跨线程共享
3. **渐进安全**：`let ref` + arena 逃逸检测（E0306）、`shared` 合约堆警告（E0502）在类型检查期捕获常见错误
4. **weak/strong 分离**：循环引用不会泄漏，`upgrade()` 的 CAS 保证线程安全

### 负面

1. **运行时开销**：RC 的原子操作在单线程场景不必要（`MIMI_NO_STD` 模式用非 atomic 计数器缓解）
2. **缺少借用检查**：当前仅按变量名追踪（`src/core/checker/borrow.rs`），复合类型借用违规在运行时才暴露
3. **Arena 有限**：`arena` 仅支持栈式生命周期，不支持跨函数借用
4. **RC 循环**：`weak` 需要程序员显式管理，无自动检测

### 缓解措施

- `MIMI_NO_STD` 编译标志移除原子操作（`src/runtime/mimi_runtime.c:236-277`）
- 类型检查器的 `E0306` 和 `E0502` 在编译期捕获 arena 逃逸和合约堆误用
- v0.15 计划引入路径敏感 borrow checker，缩小运行时风险面
- `#[ignore]` 测试（8 个）追踪已知 borrow boundary 差距（`src/tests/borrow_boundary.rs`）

---

## 参考

- 解释器共享变量分配：`src/interp/eval/stmt.rs:413-421`
- Arena 分配与逃逸检测：`src/interp/eval/stmt.rs:56-93, 205-230`
- Codegen RC 分配：`src/codegen/mod.rs:425-521`
- Codegen scope 释放：`src/codegen/scope.rs:170-243`
- Codegen weak/upgrade：`src/codegen/expr/call/method.rs:22-166`
- C runtime RC 实现：`src/runtime/mimi_runtime.c:239-449`
- 类型检查 borrow 追踪：`src/core/checker/borrow.rs`
- Arena 逃逸检测（E0306）：`src/core/check_stmt.rs:512-524`
- Borrow boundary 已知差距：`src/tests/borrow_boundary.rs`
- v0.15 计划：`AGENTS.md §12`
