# 02 - Mimi 类型系统

---

## 1. 基础类型

| 类型 | 说明 | 大小 | Copy |
|------|------|------|------|
| `i32` | 有符号 32 位整数 | 4 字节 | ✅ |
| `i64` | 有符号 64 位整数 | 8 字节 | ✅ |
| `f64` | 64 位浮点数 | 8 字节 | ✅ |
| `bool` | 布尔值 | 1 字节 | ✅ |
| `string` | UTF-8 不可变字符串 | 动态 | ❌ |
| `unit` | 空元组 `()` | 0 字节 | ✅ |
| `nothing` | 不可达 / 错误类型 | - | - |

### Copy 类型

以下类型自动实现 Copy（赋值时复制而非移动）：
- `i32`, `i64`, `f64`, `bool`, `unit`
- 所有字段都是 Copy 的记录类型
- Copy 元组

非 Copy 类型赋值时发生 Move：

```mimi
let a = 42;       // i32, Copy
let b = a;        // 复制，a 仍可用
a + b             // OK

let s = "hello";  // string, Move
let t = s;        // s 被移动
// s              // 编译错误：use of moved value
```

---

## 2. 复合类型

### 2.1 元组

```mimi
let t = (1, "hello", true);   // (i32, string, bool)
let (a, b, c) = t;            // 解构
let first = t.0;              // 索引访问
```

### 2.2 列表（动态数组）

```mimi
let nums = [1, 2, 3];         // List<i32>
let empty: List<i32> = [];    // 空列表

// 内置操作
push(nums, 4);                // 追加
let (last, rest) = pop(nums); // 弹出最后一个
contains(nums, 2);            // 包含检查
len(nums);                    // 长度
```

### 2.3 数组（固定大小）

```mimi
let arr = [1, 2, 3];          // [i32; 3]
let arr2: [i32; 5] = [0; 5]; // 5 个 0
```

### 2.4 切片

```mimi
let arr = [1, 2, 3, 4, 5];
let slice = arr[1..4];        // [2, 3, 4]
let first_three = arr[..3];   // [1, 2, 3]
let from_two = arr[2..];      // [3, 4, 5]
```

---

## 3. 记录（Record / Struct）

```mimi
type Point {
    x: f64
    y: f64
}

type User {
    name: string
    age: i32
    email: string
}
```

### 创建与访问

```mimi
let p = Point { x: 1.0, y: 2.0 };
let name = p.x;            // 字段访问
p.x = 3.0;                 // 可变字段需要 mut

type MutablePoint {
    mut x: f64
    mut y: f64
}
```

### 带 mms 块的记录（已移除）

> 0.1.8 Phase E：`mms{...}` 不再解析，MimiSpec 已拆离。
> 类型定义只包含普通字段/变体。

---

## 4. 枚举（ADT）

### 4.1 基本枚举

```mimi
type Direction {
    North
    South
    East
    West
}
```

### 4.2 带数据的枚举

```mimi
type Shape {
    Circle(f64)                          // 匿名字段
    Rectangle(f64, f64)                  // 多个匿名字段
    Triangle { a: f64, b: f64, c: f64 } // 具名字段
}
```

### 4.3 Option 与 Result

```mimi
// Option 用 ? 后缀表示
type Option<T> {
    Some(T)
    None
}

// Result
type Result<T, E> {
    Ok(T)
    Err(E)
}
```

### 4.4 使用枚举

```mimi
let s = Shape::Circle(5.0);
let r = match s {
    Shape::Circle(r) => 3.14 * r * r,
    Shape::Rectangle(w, h) => w * h,
    Shape::Triangle { a, b, c } => {
        let s = (a + b + c) / 2.0;
        (s * (s - a) * (s - b) * (s - c)).sqrt()
    }
};
```

### 4.5 穷尽性检查

编译器强制要求 match 覆盖所有变体：

```mimi
type Color { Red, Green, Blue }

func name(c: Color) -> string {
    match c {
        Color::Red => "red",
        Color::Green => "green",
        // 编译错误：未覆盖 Blue
    }
}
```

使用 `_` 通配符匹配剩余情况：

```mimi
func name(c: Color) -> string {
    match c {
        Color::Red => "red",
        _ => "other"
    }
}
```

---

## 5. 类型别名与 newtype

### 5.1 类型别名（透明）

```mimi
type Meter = f64;
type UserId = i64;

// 完全透明，可互换
let distance: Meter = 100.0;
let raw: f64 = distance;   // OK
```

### 5.2 newtype（强隔离）

```mimi
newtype UserId = u64;
newtype Meter = f64;

let id: UserId = UserId(42);
// let raw: u64 = id;      // 编译错误：类型不匹配
let raw: u64 = id.0;       // 需要显式解包
```

newtype 的意义：
- 防止不同类型混淆（如 UserId 和 OrderId 都是 u64）
- 可以为 newtype 实现独立的方法和 trait
- 运行时零开销

---

## 6. 泛型

### 6.1 泛型函数

```mimi
func identity<T>(x: T) -> T {
    x
}

func first<T>(list: List<T>) -> T {
    list[0]
}

func pair<A, B>(a: A, b: B) -> (A, B) {
    (a, b)
}
```

### 6.2 泛型类型

```mimi
type Pair<A, B> {
    first: A
    second: B
}

type Container<T> {
    value: T
    count: i32
}
```

### 6.3 where 约束

```mimi
func print_item<T>(item: T) where T: Display {
    println(to_string(item));
}

func sort_list<T>(list: List<T>) where T: Comparable {
    // ...
}

func process<T>(x: T) where T: Display + Clone {
    let copy = clone(x);
    println(to_string(copy));
}
```

### 6.4 Turbofish 语法

```mimi
let result = identity::<i32>(42);
let f = first::<string>(["a", "b", "c"]);
```

---

## 7. Trait 与 Impl

### 7.1 定义 trait

```mimi
trait Display {
    func to_string() -> string;
}

trait Comparable {
    func compare_to(other: Self) -> i32;
}

trait Summable {
    func sum() -> i32;
}
```

### 7.2 实现 trait

```mimi
impl Display for User {
    func to_string() -> string {
        "User(" + self.name + ")"
    }
}

impl Display for Point {
    func to_string() -> string {
        "Point(" + to_string(self.x) + ", " + to_string(self.y) + ")"
    }
}

impl Comparable for User {
    func compare_to(other: User) -> i32 {
        if self.age < other.age { -1 }
        else if self.age > other.age { 1 }
        else { 0 }
    }
}
```

### 7.3 derive 宏

```mimi
#[derive(Debug, Clone, Eq)]
type User {
    name: string
    age: i32
}
```

自动生成：
- `to_string()` — Debug 格式输出
- `clone()` — 深拷贝
- `eq(other)` — 相等比较

---

## 8. 可选值（Option）

Mimi 中用 `T?` 表示可选值，`null` 不是任意类型的子类型。

```mimi
func find_user(id: i64) -> User? {
    // 返回 Some(user) 或 None
}

// 使用
match find_user(42) {
    Some(user) => println(user.name),
    None => println("not found")
}

// 使用 ? 运算符
let user = find_user(42)?;   // None 时提前返回 None
```

---

## 9. 类型推断

Mimi 支持类型推断，大部分场景无需显式标注：

```mimi
let x = 42;           // 推断为 i32
let y = 3.14;         // 推断为 f64
let s = "hello";      // 推断为 string
let list = [1, 2, 3]; // 推断为 List<i32>
let (a, b) = (1, "x"); // 推断为 (i32, string)

// 需要显式标注的场景
let empty: List<i32> = [];
let num = 42i64;       // 指定 i64 类型
```

---

## 10. 类型转换

```mimi
let x: i32 = 42;
let y: f64 = to_float(x);     // i32 → f64
let z: string = to_string(x); // i32 → string
let n: i64 = to_int("42");    // string → i64
```

---

## 11. 引用类型

### 11.1 不可变引用

```mimi
let x = 10;
let r = &x;      // r 是 &i32
println(*r);     // 解引用：10
```

### 11.2 可变引用

```mimi
let mut x = 10;
let r = &mut x;   // r 是 &mut i32
*r = 20;          // 通过可变引用修改
```

### 11.3 借用规则

- 任意时刻：要么有多个不可变引用，要么只有一个可变引用
- 引用不能比被引用值活得更久

```mimi
let mut x = 10;
let r1 = &x;     // OK：不可变引用
let r2 = &x;     // OK：多个不可变引用
// let r3 = &mut x;  // 错误：已有不可变引用
```

---

## 12. 共享所有权

### 12.1 shared（原子 Arc）

```mimi
let data = shared [1, 2, 3];
let clone1 = data.clone();   // 引用计数 +1
let clone2 = data.clone();   // 引用计数 +1
```

### 12.2 local_shared（非原子 Rc）

```mimi
let data = local_shared [1, 2, 3];
// 仅限单线程使用
```

### 12.3 weak 引用

```mimi
let shared_data = shared [1, 2, 3];
let weak_ref = weak shared_data;

// 升级为 shared
match weak_ref.upgrade() {
    Some(data) => println(data),
    None => println("已释放")
}
```

---

## 13. Arena 区域内存

```mimi
func process(req: Request) -> Response {
    arena {
        let ref temp = build_huge_graph(req);
        let result = analyze(temp);

        // ref 不能逃逸 arena 块
        // global_cache.push(temp);  // 编译错误

        // 必须提取值
        Response { data: result.copy() }
    }
    // arena 块退出后，所有 ref 自动回收
}
```

规则：
- `ref T` 的生命周期 = arena 块
- 禁止将 `ref` 赋给外层变量
- 禁止将 `ref` 传给 `shared`
- 需要逃逸时使用 `.copy()` 显式拷贝

---

## 14. 分配器

```mimi
// 系统分配器（默认）
let x = alloc(System, 42);

// Arena 分配器
let x = alloc(Arena, 42);

// Bump 分配器
let x = alloc(Bump, 42);

// 使用块语法
alloc(Arena) {
    let ref temp = allocate_large_buffer();
    // ...
}
```
