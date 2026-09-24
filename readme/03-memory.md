# 03 - Mimi 内存模型

---

## 1. 总览

Mimi 采用分层内存策略，遵循"默认最简单，复杂按需取"原则：

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

---

## 2. Move 语义

### 2.1 默认 Move

非 Copy 类型赋值时发生 Move，原变量不可再使用：

```mimi
let s1 = "hello";
let s2 = s1;       // s1 被 Move 到 s2
// println(s1);     // 编译错误：use of moved value
println(s2);        // OK
```

### 2.2 Copy 类型

基本类型和所有字段都是 Copy 的记录类型自动实现 Copy：

```mimi
let a = 42;       // i32, Copy
let b = a;        // 复制
println(a + b);   // OK：a 仍可用

let p = Point { x: 1.0, y: 2.0 };  // 所有字段 Copy
let q = p;                           // 复制
println(p.x + q.x);                  // OK
```

### 2.3 显式克隆

非 Copy 类型需要显式克隆：

```mimi
let s1 = "hello";
let s2 = s1.copy();    // 显式拷贝
println(s1);            // OK：s1 仍可用
```

### 2.4 use-after-move 检测

运行时检测 use-after-move：

```mimi
let s = "hello";
let t = s;
// s              // 运行时错误：use of moved value
```

---

## 3. 借用

### 3.1 不可变借用 `&T`

```mimi
let x = 10;
let r = &x;         // 不可变借用
println(*r);        // 解引用：10
println(x);         // OK：x 仍可用
```

### 3.2 可变借用 `&mut T`

```mimi
let mut x = 10;
let r = &mut x;     // 可变借用
*r = 20;            // 通过引用修改
println(x);         // 20
```

### 3.3 借用规则

**规则 1**：任意时刻，要么有多个 `&T`，要么只有一个 `&mut T`：

```mimi
let mut x = 10;

// ✅ 多个不可变借用
let r1 = &x;
let r2 = &x;

// ❌ 有不可变借用时不能创建可变借用
// let r3 = &mut x;
```

**规则 2**：可变借用期间不能有其他借用：

```mimi
let mut x = 10;
let r = &mut x;
*r = 20;
// let r2 = &x;    // 错误：可变借用仍活跃
```

**规则 3**：引用的生命周期不能超过被引用值：

```mimi
let r;
{
    let x = 10;
    r = &x;         // 错误：x 即将被销毁
}
```

### 3.4 方法中的 self 借用

```mimi
type Counter {
    mut value: i32
}

impl Counter {
    // 不可变借用 self
    func get(&self) -> i32 {
        self.value
    }

    // 可变借用 self
    func increment(&mut self) {
        self.value += 1;
    }

    // 消费 self
    func into_value(self) -> i32 {
        self.value
    }
}
```

---

## 4. 共享所有权

### 4.1 shared（原子 Arc）

线程安全的引用计数所有权：

```mimi
let data = shared [1, 2, 3];

// 克隆增加引用计数
let clone1 = data.clone();
let clone2 = data.clone();

// 所有引用计数归零时自动释放
drop(clone1);
drop(clone2);
drop(data);         // 最后一个，释放内存
```

### 4.2 local_shared（非原子 Rc）

单线程的引用计数所有权：

```mimi
let data = local_shared [1, 2, 3];
let clone = data.clone();
// 仅限单线程使用
```

### 4.3 weak 弱引用

不增加引用计数，防止循环引用：

```mimi
type Node {
    value: i32,
    children: List<shared Node>
}

let root = shared Node { value: 1, children: [] };
let child = shared Node { value: 2, children: [] };

// weak 不增加引用计数
let weak_child = weak child;

// 升级为 shared（可能失败）
match weak_child.upgrade() {
    Some(node) => println(node.value),
    None => println("已释放")
}
```

### 4.4 内部可变性

shared 对象默认只读，需要可变字段时使用 `mut`：

```mimi
type AppState {
    mut counter: i32 = 0,
    name: string
}

let state = shared AppState { counter: 0, name: "app".into() };
state.clone().counter += 1;  // 通过 shared 修改 mut 字段
```

---

## 5. Arena 区域内存

Arena 提供批量分配、批量释放的区域内存：

```mimi
func handle_request(req: Request) -> Response {
    arena {
        let ref graph = build_graph(req);      // 在 arena 中分配
        let ref analysis = analyze(graph);     // 在 arena 中分配

        // 使用 arena 中的数据
        let result = summarize(analysis);

        // 返回时必须提取值，不能返回 ref
        Response { data: result.copy() }
    }
    // arena 块退出后，graph 和 analysis 自动回收
}
```

### 5.1 ref 类型

`ref T` 是 arena 内部的引用，生命周期 = arena 块：

```mimi
arena {
    let ref x = 10;         // ref i32
    let ref s = "hello";    // ref string

    println(*x);            // 解引用
    *x = 20;                // 通过 ref 修改

    // ❌ ref 不能逃逸
    // let outer = x;

    // ❌ ref 不能传给 shared
    // let s = shared x;

    // ✅ 需要逃逸时显式拷贝
    let owned = x.copy();
}
```

### 5.2 Arena 的优势

- **批量释放**：退出 arena 块时一次性释放所有对象，无碎片
- **无 GC 暂停**：确定性释放，无垃圾回收器介入
- **缓存友好**：arena 内对象在内存中连续排列

---

## 6. 线性能力 (cap)

### 6.1 声明 cap

```mimi
cap FileReadCap;
cap FileWriteCap;
cap NetConnectCap;
```

### 6.2 cap 的性质

- **不可复制**：赋值时 Move，不 Copy
- **不可隐式丢弃**：必须显式消费或传递
- **必须在所有控制流路径上被消费**

```mimi
func read_file(path: string, cap: FileReadCap) -> string {
    let data = std::fs::read(path, cap);  // cap 被消费
    data
}
```

### 6.3 组合 cap

使用 `+` 组合多个 cap：

```mimi
cap FullFileAccess = FileReadCap + FileWriteCap;
cap FullNetAccess = NetConnectCap + NetListenCap;
```

### 6.4 分解 cap

使用 `.split()` 分解组合 cap：

```mimi
func task(full: FullFileAccess) {
    let (read_cap, write_cap) = full.split();
    // 分别使用
    do_read(read_cap);
    do_write(write_cap);
}
```

### 6.5 释放 cap

```mimi
func release(cap: FileWriteCap) {
    drop(cap);   // 显式释放
}
```

### 6.6 与 FFI 的边界

线性 `cap` 可以由 Mimi 函数签名表达，但当前 Canonical MIR 导入 profile 不支持把 cap、字符串、指针或聚合值当作 C ABI 参数传递。外部导入目前只接受 `extern "C"` 标量参数（`i32`、`i64`、`f32`、`f64`、`bool`）以及标量或 unit 返回值。不要用 Mimi FFI 声明模拟带能力参数的 SQLite、文件或 socket C API；网络功能请查看 `std::net`，准确的 FFI 边界与链接方式见[FFI 指南](./10-ffi.md)。

---

## 7. 内存模型对比

| 特性 | Rust | Mimi |
|------|------|------|
| 所有权 | Move + 借用检查器 | Move + 基本借用检查 |
| 多所有权 | `Arc<T>` / `Rc<T>` | `shared T` / `local_shared T` |
| 内部可变性 | `RefCell<T>` / `Mutex<T>` | `mut` 字段 + 内部锁 |
| 弱引用 | `Weak<T>` | `weak T` |
| 区域内存 | 手动生命周期 | `arena { ref T }` |
| 权限控制 | 无内建 | `cap` 线性类型 |
| GC | 无 | 无 |
| 学习曲线 | 高 | 中 |

Mimi 的内存模型在保持安全性的前提下，显著降低了心智负担。
