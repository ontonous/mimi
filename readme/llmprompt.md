# LLM Prompt：让 AI 正确编写 Mimi 代码

> ⚠ **文档状态（0.34.33 标注）**：本文档主体写于 v0.7.0 时代，语言自 0.29 起已演进为
> **Flow-first typestate** 范式并经 0.1.4 语法冻结。下文仅 **§2.4 关键字表已同步到冻结实况**，
> 其余章节（意图后缀、部分语法示例）可能滞后。权威语法以 `docs/language-spec.md` +
> `docs/syntax-reference.md`（golden EBNF 渲染副本）为准；Flow/transition 语法见
> `tests/real_world/` 语料与 README Hello Flow 示例。全文重写已登记（devdocs/v0.34 审计）。
>
> **目标**：本指南面向需要生成 `.mimi` 代码的大语言模型。读完后，你应能独立生成**语法合法、类型正确、惯用风格**的 Mimi 程序。
>
> **版本**：快照基于 Mimi v0.7.0（v1.2 集成版）；关键字表已按 0.1.4-dev（0.34）冻结实况刷新。
> **输出要求**：只输出 `.mimi` 源码文本，不要用 Markdown 代码块包裹（除非用户要求）。

---

## 1. Mimi 是什么

Mimi 是一门面向 **Intent-as-Code + Safe AI Collaboration** 的系统编程语言。它的核心特点：

- **花括号体** `{ ... }` — 函数、模块、actor 等都用花括号
- **Move 语义** — 值默认移动，基本类型自动 Copy
- **契约式编程** — `requires`/`ensures` 直接写在函数签名后
- **意图后缀** — `$`/`$$` 标记人类锁定，`?`/`??` 标记 AI 可修改区域
- **Actor 并发** — 轻量级有状态并发实体
- **线性能力** — `cap` 类型控制敏感权限

---

## 2. 绝对规则（Parser 硬约束）

违反以下任何一条，文件都会解析失败。

### 2.1 花括号体

`.mimi` 文件中，函数体、模块体、actor 体、if/else/while/for 的主体**必须用花括号** `{ ... }`：

```mimi
// ✅ 正确
func add(a: i32, b: i32) -> i32 {
    a + b
}

// ❌ 错误：Mimi 不用缩进体（`.mms` 草图模式已于 0.1.8 移除）
func add(a: i32, b: i32) -> i32:
    a + b
```

### 2.2 缩进

- 缩进单位必须是 **4 个空格的整数倍**（4、8、12...）。
- **禁止使用 Tab**。
- 花括号内的语句按缩进层级对齐。

### 2.3 字符串

- 使用双引号 `"..."`。
- **禁止隐式跨行**：未闭合引号遇到换行会报 `unterminated string`。
- 需要物理换行时使用转义 `\n`。
- 支持转义：`\\`、`\"`、`\n`、`\t`、`\r`。
- 支持 f-string：`f"Hello, {name}!"`。

### 2.4 关键字不可用作标识符

> 0.35.39 刷新：0.1.5 实况，共 67 个 `=> TokenKind` 映射（实测 keywords.rs `keyword_or_ident`）。
> 已删除：`do`/`become`/`stay`/`subflow`/`steps`/`consume`（现 tokenize 为标识符）；
> 0.35.39 裁撤 `c_shared`/`c_borrow`/`c_borrow_mut`/`local_shared`/`weak_local`/`raw_string`/
> `nothing`/`alloc`/`async`/`with`/`desc`/`rule`/`mms`（共享收敛为 `shared`/`weak` 二态）。
> 0.1.8 Phase E：`desc`/`rule`/`mms` 不再是超注释或可嵌入块；`.mms` 文件与
> `mimi mms`/`mimi promote` 已移除。

```
module     type       func       fn         actor      newtype
trait      impl       cap        extern     use        pub
dyn        where
let        mut        ref        const      shared     weak
arena      drop
if         else       for        in         while      loop
return     break      continue   match      defer      unsafe
flow       state      transition fault      fails      persistent
pinned     protocol   session    dual       end        view
mutate
spawn      await      parasteps  failure    reset      recover
requires   ensures    invariant  math       old
comptime   quote
and        or         not        (软关键字：绑定位置可作标识符)
true       false      unit
as
```

### 2.5 冒号规则

以下结构名字/签名后**必须**带 `:`：

```mimi
module Name:
type Name:              // 或 type Name: A | B（枚举）
```

**在 .mimi 生产模式中**，`func`、`if`、`else`、`for`、`while` 使用花括号，不带冒号。
`.mms` 草图模式（`flow Name:`/`func Name(...):`/`ui Name`/`steps:`）已于 0.1.8 移除。

### 2.6 意图后缀（已移除）

> 0.1.8 Phase E：MimiSpec 的意图后缀 `$`/`$$`/`?`/`??` 已从语言拆离。
> Mimi 编译器不识别这些后缀。

```mimi
func$$ critical() { ... }     // ✅ 强锁定
func? uncertain() { ... }     // ✅ 不确定
func$? locked_review() { ... }// ✅ 锁定但 AI 可审视
func?$ wrong() { ... }        // ❌ 非法顺序
```

---

## 3. 程序结构

### 3.1 最小程序

```mimi
func main() -> i32 {
    println("Hello, Mimi!");
    0
}
```

- `main` 函数是程序入口
- 返回 `i32`（退出码），无返回值时省略返回类型
- 最后一个表达式可省略 `return`

### 3.2 多文件程序

```
src/
├── main.mimi       // 入口
├── models.mimi     // 数据模型
└── utils.mimi      // 工具函数
```

```mimi
// main.mimi
use crate::models::User;
use crate::utils::format_user;

func main() -> i32 {
    let user = User { name: "Alice".into(), age: 30 };
    println(format_user(user));
    0
}
```

---

## 4. 类型系统

### 4.1 基础类型

| 类型 | 说明 | Copy |
|------|------|------|
| `i32` | 32 位整数 | ✅ |
| `i64` | 64 位整数 | ✅ |
| `f64` | 64 位浮点 | ✅ |
| `bool` | 布尔 | ✅ |
| `string` | UTF-8 字符串 | ❌ |
| `unit` | 空元组 `()` | ✅ |
| `nothing` | 不可达类型 | - |

### 4.2 记录（struct）

```mimi
type Point {
    x: f64
    y: f64
}

// 字段间可用逗号或换行分隔
type User {
    name: string,
    age: i32,
}
```

### 4.3 枚举（ADT）

```mimi
type Shape {
    Circle(f64)                          // 匿名字段
    Rectangle(f64, f64)                  // 多个匿名字段
    Triangle { a: f64, b: f64, c: f64 } // 具名字段
}
```

### 4.4 类型别名与 newtype

```mimi
type Meter = f64;          // 透明别名，与 f64 完全互换
newtype UserId = u64;      // 强类型隔离，不可与 u64 互换
```

### 4.5 泛型

```mimi
type Pair<A, B> {
    first: A
    second: B
}

func identity<T>(x: T) -> T { x }
func first<T>(list: List<T>) -> T { list[0] }
```

### 4.6 Trait 与 Impl

```mimi
trait Display {
    func to_string() -> string;
}

impl Display for Point {
    func to_string() -> string {
        "Point(" + to_string(self.x) + ", " + to_string(self.y) + ")"
    }
}
```

### 4.7 derive 宏

```mimi
#[derive(Debug, Clone, Eq)]
type User {
    name: string
    age: i32
}
```

自动实现 `to_string()`、`clone()`、`eq()`。

### 4.8 where 约束

```mimi
func print_item<T>(item: T) where T: Display {
    println(to_string(item));
}
```

---

## 5. 变量与内存

### 5.1 let 绑定

```mimi
let x = 42;             // 不可变
let mut y = 20;         // 可变
let (a, b) = (1, 2);   // 解构
```

### 5.2 Move 语义

```mimi
let a = 42;       // i32, Copy → 赋值复制
let b = a;        // a 仍可用

let s = "hello";  // string, Move → 赋值移动
let t = s;        // s 不再可用
```

### 5.3 借用

```mimi
let mut x = 10;
let r = &mut x;   // 可变借用
*r = 20;
```

### 5.4 共享所有权

```mimi
let data = shared [1, 2, 3];     // 线程安全 Arc
let data = local_shared [1, 2];  // 单线程 Rc
let w = weak data;               // 弱引用
```

### 5.5 Arena 区域内存

```mimi
func process() -> i32 {
    arena {
        let ref temp = compute();  // ref 生命周期 = arena 块
        result.copy()              // 需要逃逸时显式拷贝
    }
}
```

### 5.6 Cap 线性能力

```mimi
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;  // 组合

func read(path: string, cap: FileReadCap) -> string { ... }

func task(full: FullAccess) {
    let (r, w) = full.split();  // 分解
    drop(r);                     // 释放
    drop(w);
}
```

---

## 6. 控制流

### 6.1 if / else

```mimi
if condition {
    // ...
} else if other {
    // ...
} else {
    // ...
}

// if 作为表达式
let x = if a > b { a } else { b };
```

### 6.2 while

```mimi
while i < 10 {
    i += 1;
    if i == 5 { continue; }
    if i == 8 { break; }
}
```

### 6.3 for-in

```mimi
for item in list {
    println(item);
}

for i in range(0, 10) {
    println(i);
}
```

### 6.4 match

```mimi
match value {
    42 => "the answer",
    x if x > 0 => "positive",
    Shape::Circle(r) => 3.14 * r * r,
    _ => "default",
    (a, b) => a + b,
    [1, 2, rest..] => rest,
}
```

match 支持：字面量、变量绑定、守卫 `if`、构造器解构、元组、数组、切片。
编译器强制穷尽性检查。

### 6.5 列表推导

```mimi
let squares = [x * x for x in range(0, 10)];
let evens = [x for x in range(0, 20) if x % 2 == 0];
```

---

## 7. 函数

### 7.1 基本函数

```mimi
func add(a: i32, b: i32) -> i32 {
    a + b    // 隐式返回
}

func greet(name: string) {
    println("Hello, " + name);  // 无返回值
}
```

### 7.2 带契约的函数

```mimi
func withdraw(mut account: Account, amount: f64) -> Result<(), string> {
    requires: account.balance >= amount
    ensures: account.balance == old(account.balance) - amount

    account.balance -= amount;
    Ok(())
}
```

- `requires`：前置条件
- `ensures`：后置条件，`old(x)` 引用入口值，`result` 指代返回值
- 契约写在函数签名后、花括号体前

### 7.3 闭包

```mimi
let double = fn(x: i32) -> i32 { x * 2 };
let add = fn(a: i32, b: i32) -> i32 { a + b };

let result = map([1, 2, 3], double);        // [2, 4, 6]
let filtered = filter([1, 2, 3], fn(x: i32) -> bool { x > 1 });
let sum = reduce([1, 2, 3], add, 0);        // 6
```

### 7.4 函数作为值

```mimi
func double(x: i32) -> i32 { x * 2 }

func apply(f: func(i32) -> i32, x: i32) -> i32 { f(x) }
let result = apply(double, 5);  // 10
```

### 7.5 comptime 函数

```mimi
comptime func make_const(name: string, value: i32) -> AST {
    quote! {
        const $(name): i32 = $(value);
    }
}
```

---

## 8. 错误处理

### 8.1 Result 类型

```mimi
type Result<T, E> {
    Ok(T)
    Err(E)
}

func divide(a: f64, b: f64) -> Result<f64, string> {
    if b == 0.0 { Err("division by zero") }
    else { Ok(a / b) }
}
```

### 8.2 ? 运算符

`?` 在 Result 为 Err 时提前返回错误：

```mimi
func process() -> Result<i32, string> {
    let n = parse_int("42")?;   // 失败时返回 Err
    Ok(n * 2)
}
```

### 8.3 on failure 补偿

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

失败时按 LIFO 逆序执行补偿块。

---

## 9. 并发

### 9.1 Actor（业务状态活在 Flow）

> 0.1.8 Phase D 废止业务 `mut` 字段：业务状态必须活在 Flow 中，由
> `actor Counter runs Counter` 包装。

```mimi
flow Counter {
    state Ready { count: i32 }
    transition increment(Ready) -> Ready {
        return Ready { count: self.count + 1 }
    }
    transition get(Ready) -> Ready {
        return Ready { count: self.count }
    }
}

actor Counter runs Counter {
    func increment() { self.increment() }
    func get() -> i32 { self.get().count }
}

func main() -> i32 {
    let c = Counter.spawn();

    // 直接调用：同步执行（内部线程）
    c.increment();
    c.increment();
    let n = c.get();  // 2

    // 异步调用：spawn + await
    let future = spawn c.increment();
    await future;

    0
}
```

**关键区别**：
- `c.method()` — 同步调用，阻塞等待结果
- `spawn c.method()` — 异步调用，返回 Future
- `await future` — 等待 Future 完成

### 9.2 Parasteps

```mimi
func load() -> string {
    parasteps "并行加载" {
        let a = spawn fetch("api/users");
        let b = spawn fetch("api/orders");
        let r1 = await a;
        let r2 = await b;
        r1 + r2
    }
}
```

### 9.3 Spawn 与 Await

```mimi
let future = spawn async_func(args);  // 创建 Future
let result = await future;            // 等待结果
```

---

## 10. 内置函数

| 函数 | 签名 | 说明 |
|------|------|------|
| `println(args...)` | any -> unit | 打印并换行 |
| `print(args...)` | any -> unit | 打印不换行 |
| `assert(cond)` | bool -> unit | 断言 |
| `assert_eq(a, b)` | (any, any) -> unit | 断言相等 |
| `assert_ne(a, b)` | (any, any) -> unit | 断言不等 |
| `range(start, end)` | (i32, i32) -> List | 整数范围 |
| `len(x)` | string/List -> i64 | 长度 |
| `push(list, elem)` | (List, T) -> List | 追加 |
| `pop(list)` | List -> (T, List) | 弹出最后一个 |
| `contains(c, elem)` | (List/string, T) -> bool | 包含检查 |
| `map(list, f)` | (List, Fn) -> List | 映射 |
| `filter(list, f)` | (List, Fn) -> List | 过滤 |
| `reduce(list, f, init)` | (List, Fn, T) -> T | 归约 |
| `sort(list)` | List -> List | 排序 |
| `reverse(list)` | List -> List | 反转 |
| `flatten(list)` | List -> List | 展平 |
| `zip(a, b)` | (List, List) -> List | 配对 |
| `enumerate(list)` | List -> List | 加索引 |
| `sum(list)` | List -> number | 求和 |
| `sqrt(x)` | number -> f64 | 平方根 |
| `abs(x)` | number -> number | 绝对值 |
| `min(a, b)` | (num, num) -> num | 最小值 |
| `max(a, b)` | (num, num) -> num | 最大值 |
| `pow(b, e)` | (num, num) -> num | 幂 |
| `floor(x)` | number -> number | 向下取整 |
| `ceil(x)` | number -> number | 向上取整 |
| `round(x)` | number -> number | 四舍五入 |
| `random()` | () -> f64 | [0,1) 随机数 |
| `pi()` | () -> f64 | π |
| `to_string(x)` | any -> string | 转字符串 |
| `to_int(x)` | any -> i64 | 转整数 |
| `to_float(x)` | any -> f64 | 转浮点 |
| `input()` | () -> string | 读取标准输入 |
| `read_file(path)` | string -> string | 读文件 |
| `write_file(path, content)` | (string, string) -> unit | 写文件 |
| `file_exists(path)` | string -> bool | 文件是否存在 |
| `str_char_at(s, i)` | (string, i64) -> string | 取字符 |
| `str_substring(s, start, end)` | (string, i64, i64) -> string | 子串 |
| `str_parse_int(s)` | string -> (bool, i64) | 解析整数 |
| `str_parse_float(s)` | string -> (bool, f64) | 解析浮点 |
| `keys(record)` | Record -> List | 键列表 |
| `values(record)` | Record -> List | 值列表 |
| `has_key(record, key)` | (Record, string) -> bool | 键是否存在 |
| `type_name(x)` | any -> string | 运行时类型名 |
| `type_fields(name)` | string -> List | 类型字段名 |
| `type_variants(name)` | string -> List | 类型变体名 |
| `ast_dump(quoted)` | QuoteAst -> string | 转储 AST |
| `ast_eval(quoted)` | QuoteAst -> Value | 求值 AST |

---

## 11. FFI（extern "C"）

```mimi
cap SQLiteCap;

extern "C" {
    func sqlite3_open(path: string, cap @db: SQLiteCap) -> Result<i64, string>;
    func sqlite3_exec(db: i64, query: string, cap @db: SQLiteCap) -> Result<(), string>;
}
```

- `cap @param: Type` — 移动语义的 cap
- `&param: Type` — 借用语义的 cap

---

## 12. MimiSpec 集成（已移除）

> 0.1.8 Phase E：`mms{}` 块不再是元数据，MimiSpec 已从 Mimi 拆离。
> 在 `.mimi` 中写 `mms {}` 会直接报解析错误。

---

## 13. 常见错误

| ❌ 错误 | ✅ 正确 |
|--------|--------|
| `func add(a, b):` | `func add(a: i32, b: i32) -> i32 { ... }` |
| `module Shop` | `module Shop { ... }` |
| `type Point: x: f64` | `type Point { x: f64 }` |
| `if x > 0:` | `if x > 0 { ... }` |
| `for i in 0..10:` | `for i in range(0, 10) { ... }` |
| `await counter.increment()` | `counter.increment()`（同步调用无需 await） |
| `@cap FileReadCap` | `cap @fh: FileReadCap`（extern 块中） |
| `func?$ wrong()` | `func$? correct()`（先锁后疑） |
| `string s = "hi"` | `let s = "hi"`（let 绑定） |

---

## 14. 编写提示

1. **始终从 `func main() -> i32` 开始**，确保程序可运行。
2. **类型标注**：变量可省略类型（编译器推断），但函数参数和返回值必须标注。
3. **错误处理**：优先使用 `?` 运算符，避免嵌套 match。
4. **Actor 方法**：直接调用是同步的，不需要 `await`；只有 `spawn` 创建的 Future 才需要 `await`。
5. **契约**：`requires`/`ensures` 写在函数签名后、花括号体前，不要写在花括号内。
6. **cap**：在 extern 块中使用 `cap @param: Type` 语法，不是 `@cap Type`。
7. **模式匹配**：match 必须穷尽所有变体，使用 `_` 通配符处理剩余情况。
8. **闭包**：使用 `fn(params) -> Ret { body }` 语法，可赋值给变量或传给高阶函数。
