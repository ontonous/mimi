# 01 - Mimi 语法手册

> 本文档是 Mimi 语言的完整语法参考。

---

## 1. 文件与注释

### 文件扩展名

| 扩展名 | 模式 | 说明 |
|--------|------|------|
| `.mimi` | Production | 生产模式，支持完整语法，函数体必须用花括号 |
| `.mms` | Removed in 0.1.8 | MimiSpec 草图模式已从 Mimi 拆离；`mms{}` 为硬错误 |

### 注释

```mimi
// 单行注释
/* 多行
   注释 */
```

### 字符串字面量

```mimi
"hello world"           // 普通字符串
"line1\nline2"          // 转义字符: \n \t \r \\ \"
```

支持的转义序列：`\\`、`\"`、`\n`、`\t`、`\r`

### f-string（格式化字符串）

```mimi
let name = "Mimi";
let version = 42;
f"Hello, {name}! Version: {version}"
```

f-string 中 `{expr}` 会被求值并插入到字符串中。

---

## 2. 关键字

> **0.35.39 刷新**：以下为 0.1.5 实况关键字表（共 **67 个** `=> TokenKind`
> 映射，实测 `src/lexer/keywords.rs` `keyword_or_ident`；其中 `and`/`or`/`not` 为软关键字）。
> 权威来源：`docs/syntax-reference.md`（golden EBNF 渲染副本）。
> 已删除的关键字：`do`（0.34.27）、`become`/`stay`（0.34.11）、`subflow`/`steps`/`consume`
> （0.34.2）——现均 tokenize 为普通标识符；`delegate` 软化为标识符（语句位置保留拒绝诊断）；
> 僵尸关键字裁撤（0.35.39）：`c_shared`/`c_borrow`/`c_borrow_mut`/`local_shared`/`weak_local`/
> `raw_string`/`nothing`/`alloc`/`async`(top-level)/`with`/`desc`/`rule`/`mms`——共享四态收敛为
> `shared`/`weak` 二态，`Type::Nothing` 保留为语义残差类型（无关键字）。

**声明与定义**
```
module     type       func       fn         actor      newtype
trait      impl       cap        extern     use        pub
dyn        where
```

**变量与内存**
```
let        mut        ref        const      shared     weak
arena      drop
```

**控制流**
```
if         else       for        in         while      loop
return     break      continue   match      defer      unsafe
```

**Flow 状态机**
```
flow       state      transition fault      fails      persistent
pinned     protocol   session    dual       end        view
mutate
```

**并发与恢复**
```
spawn      await      parasteps  failure    reset      recover
```

**契约与元数据**
```
requires   ensures    invariant  math       old
```

**元编程**
```
comptime   quote
```

**逻辑运算符——软关键字**（绑定位置可作标识符）
```
and        or         not
```

**字面量**
```
true       false      unit
```

**其它**
```
as
```

> 注：`i32`/`i64`/`f64`/`bool`/`string` 是内建类型名，tokenize 为标识符，不是关键字。

---

## 3. 字面量

### 整数

```mimi
42          // i32 (默认)
100i64      // i64
0xff        // 十六进制
0b1010      // 二进制
0o77        // 八进制
```

### 浮点数

```mimi
3.14        // f64 (默认)
1.0e10      // 科学计数法
```

### 布尔

```mimi
true
false
```

### 字符串

```mimi
"hello"
"hello\nworld"
```

### 单元值

```mimi
()          // unit 类型
```

### 列表

```mimi
[1, 2, 3]          // List<i32>
["a", "b", "c"]    // List<string>
```

### 元组

```mimi
(1, "hello", true)  // (i32, string, bool)
```

### 数组（固定大小）

```mimi
[1, 2, 3]          // 推断为 [i32; 3]
```

---

## 4. 标识符与命名

```mimi
let x = 10;                // 变量
let mut y = 20;            // 可变变量
let _private = 0;          // 下划线前缀（约定私有）
let camelCase = 1;         // 驼峰命名（合法但不推荐）
let snake_case = 2;        // 蛇形命名（推荐）
```

**命名约定**（编译器不强制，但推荐）：

| 类别 | 约定 | 示例 |
|------|------|------|
| 变量/函数 | snake_case | `my_func`, `let my_var` |
| 类型/ADT | PascalCase | `MyType`, `Shape` |
| 模块 | PascalCase | `module MyModule` |
| Actor | PascalCase | `actor Counter` |
| Cap | PascalCase | `cap FileReadCap` |
| Trait | PascalCase | `trait Display` |

---

## 5. 运算符

### 5.1 算术运算符

| 运算符 | 含义 | 示例 |
|--------|------|------|
| `+` | 加 | `a + b` |
| `-` | 减 | `a - b` |
| `*` | 乘 | `a * b` |
| `/` | 除 | `a / b` |
| `%` | 取模 | `a % b` |
| `**` | 幂 | `a ** b` |

### 5.2 比较运算符

| 运算符 | 含义 |
|--------|------|
| `==` | 等于 |
| `!=` | 不等于 |
| `<` | 小于 |
| `>` | 大于 |
| `<=` | 小于等于 |
| `>=` | 大于等于 |

### 5.3 逻辑运算符

| 运算符 | 含义 | 短路 |
|--------|------|------|
| `&&` / `and` | 逻辑与 | 是 |
| `\|\|` / `or` | 逻辑或 | 是 |
| `!` / `not` | 逻辑非 | 否 |

### 5.4 位运算符

| 运算符 | 含义 |
|--------|------|
| `&` | 按位与 |
| `\|` | 按位或 |
| `^` | 按位异或 |
| `<<` | 左移 |
| `>>` | 右移 |
| `~` | 按位取反 |

### 5.5 赋值运算符

| 运算符 | 含义 |
|--------|------|
| `=` | 赋值 |
| `+=` | 加后赋值 |
| `-=` | 减后赋值 |
| `*=` | 乘后赋值 |
| `/=` | 除后赋值 |

### 5.6 其他运算符

| 运算符 | 含义 |
|--------|------|
| `.` | 字段访问 / 方法调用 |
| `::` | 模块路径 |
| `?` | 错误传播（try） |
| `..` | 范围 / 切片 |
| `...` | 草图模式占位符 |
| `=>` | match 分支箭头 |
| `->` | 返回类型 |
| `@` | extern 块中的 cap 标注（如 `cap @param: Type`） |

### 5.7 运算符优先级（从低到高）

| 优先级 | 运算符 |
|--------|--------|
| 1 (最低) | `\|\|` / `or` |
| 2 | `&&` / `and` |
| 3 | `==`, `!=` |
| 4 | `<`, `>`, `<=`, `>=` |
| 5 | `\|` |
| 6 | `^` |
| 7 | `&` |
| 8 | `<<`, `>>` |
| 9 | `+`, `-` |
| 10 | `*`, `/`, `%` |
| 11 (最高) | `**` |

---

## 6. 表达式

### 6.1 基本表达式

```mimi
42              // 整数字面量
3.14            // 浮点字面量
true            // 布尔字面量
"hello"         // 字符串字面量
()              // 单元值
x               // 变量引用
func_name(args) // 函数调用
obj.field       // 字段访问
obj.method()    // 方法调用
```

### 6.2 算术与逻辑表达式

```mimi
a + b * c           // 算术
a > 0 && b < 10     // 逻辑
!flag               // 取反
```

### 6.3 if 表达式

```mimi
let x = if cond { 1 } else { 2 };
```

### 6.4 match 表达式

```mimi
let result = match value {
    Pattern1 => expr1,
    Pattern2 => expr2,
    _ => default_expr
};
```

### 6.5 闭包表达式

```mimi
let double = fn(x: i32) -> i32 { x * 2 };
let add = fn(a: i32, b: i32) -> i32 { a + b };
```

### 6.6 列表推导

```mimi
let squares = [x * x for x in range(0, 10)];
let evens = [x for x in range(0, 20) if x % 2 == 0];
```

### 6.7 try 表达式

```mimi
let value = risky_func()?;     // 失败时提前返回 Err
```

### 6.8 spawn / await 表达式

```mimi
let future = spawn async_func();   // 创建 Future
let result = await future;         // 等待结果
```

### 6.9 comptime 表达式

```mimi
comptime {
    // 编译期执行的代码
}
```

### 6.10 quote 表达式

```mimi
let ast = quote! {
    const $(name): i32 = $(value);
};
```

---

## 7. 语句

### 7.1 let 绑定

```mimi
let x = 10;             // 不可变绑定
let mut y = 20;         // 可变绑定
let (a, b) = (1, 2);   // 元组解构
```

### 7.2 赋值

```mimi
x = 10;
y += 5;
```

### 7.3 return

```mimi
return;          // 返回 ()
return expr;     // 返回 expr
```

函数体最后一个表达式可省略 `return`：

```mimi
func add(a: i32, b: i32) -> i32 {
    a + b     // 隐式返回
}
```

### 7.4 break / continue

```mimi
while true {
    break;          // 跳出循环
    break value;    // 跳出循环并返回值
    continue;       // 跳到下一次迭代
}
```

### 7.5 desc / rule / steps（已移除）

> 0.1.8 Phase E：`desc`/`rule`/`steps` 与 `mms{}` 一同移除。MimiSpec 不再嵌入
> Mimi；这些词 tokenize 为普通标识符，按普通语法位置解析或拒绝。
> 权威 removed 语法见 `docs/syntax-reference.md` §6.3 与 `docs/language-spec.md` §6.6/§6.9。

### 7.6 drop

```mimi
drop(cap);              // 显式释放 cap
drop(result);           // 忽略 Result 值
```

---

## 8. 控制流

### 8.1 if / else

```mimi
if condition {
    // ...
}

if condition {
    // ...
} else {
    // ...
}

if cond1 {
    // ...
} else if cond2 {
    // ...
} else {
    // ...
}
```

`if` 也可作为表达式：

```mimi
let x = if a > b { a } else { b };
```

### 8.2 while

```mimi
while condition {
    // ...
}

while i < 10 {
    i += 1;
    if i == 5 { continue; }
    if i == 8 { break; }
}
```

### 8.3 for-in

```mimi
for item in list {
    println(item);
}

for i in range(0, 10) {
    println(i);
}

for (i, v) in enumerate(list) {
    println(i, v);
}
```

### 8.4 match

```mimi
match value {
    // 字面量匹配
    42 => "the answer",
    "hello" => "greeting",

    // 变量绑定
    x => x,

    // 通配符
    _ => "default",

    // 构造器解构
    Some(v) => v,
    None => 0,

    // 元组解构
    (a, b) => a + b,

    // 守卫条件
    x if x > 0 => "positive",

    // 花括号体（多语句）
    Circle(r) => {
        let area = 3.14 * r * r;
        area
    }
}
```

---

## 9. 函数

### 9.1 基本函数

```mimi
func add(a: i32, b: i32) -> i32 {
    a + b
}
```

### 9.2 无返回值

```mimi
func greet(name: string) {
    println("Hello, " + name);
}
```

### 9.3 带契约的函数

```mimi
func withdraw(mut account: Account, amount: f64) -> Result<(), Err> {
    requires: account.balance >= amount
    ensures: account.balance == old(account.balance) - amount

    account.balance -= amount;
    Ok(())
}
```

### 9.4 泛型函数

```mimi
func identity<T>(x: T) -> T {
    x
}

func first<T>(list: List<T>) -> T {
    list[0]
}
```

### 9.5 带 where 约束的函数

```mimi
func print_it<T>(x: T) where T: Display {
    println(to_string(x));
}
```

### 9.6 带 effects 标注的函数

```mimi
func fetch_data(url: string) with IO, Network {
    // ...
}
```

### 9.7 comptime 函数

```mimi
comptime func make_const(name: string, value: i32) -> AST {
    quote! {
        const $(name): i32 = $(value);
    }
}
```

### 9.8 闭包

```mimi
let double = fn(x: i32) -> i32 { x * 2 };
let add = fn(a: i32, b: i32) -> i32 { a + b };

// 作为参数传递
let result = map([1, 2, 3], double);
let filtered = filter([1, 2, 3, 4], fn(x: i32) -> bool { x > 2 });
let sum = reduce([1, 2, 3], fn(acc: i32, x: i32) -> i32 { acc + x }, 0);
```

### 9.9 函数作为值

```mimi
func double(x: i32) -> i32 { x * 2 }

func apply(f: func(i32) -> i32, x: i32) -> i32 {
    f(x)
}

let result = apply(double, 5);  // 10
```

### 9.10 意图后缀（MimiSpec 专用，已移除）

> 0.1.8 Phase E 将 MimiSpec 从 Mimi 拆离，`.mms` 意图后缀不再属于语言。
> 在 `.mimi` 文件中写 `func$$` / `func?` 会按普通标识符/解析错误处理。

---

## 10. 类型定义

### 10.1 记录（struct）

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

### 10.2 枚举（ADT）

```mimi
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
    Triangle { a: f64, b: f64, c: f64 }
}
```

变体可以有：
- 无字段：`None`
- 匿名字段：`Some(i32)`
- 具名字段：`Point { x: f64, y: f64 }`

### 10.3 类型别名

```mimi
type Meter = f64;
type ID = i64;
```

### 10.4 newtype

```mimi
newtype UserId = u64;
newtype Meter = f64;
```

newtype 创建强类型隔离，与原始类型不兼容：

```mimi
let id: UserId = UserId(42);
let raw: u64 = id.0;     // 需要显式解包
```

### 10.5 泛型类型

```mimi
type Pair<A, B> {
    first: A
    second: B
}

type List<T> {
    items: [T]
}
```

### 10.6 derive 宏

```mimi
#[derive(Debug, Clone, Eq)]
type User {
    name: string
    age: i32
}
```

自动生成 `to_string()`、`clone()`、`eq()` 方法。

### 10.7 带 mms 块的类型（已移除）

> 0.1.8 Phase E：`mms{...}` 块不再解析为类型内元数据，直接硬错误。
> 类型定义只包含普通字段/变体。

---

## 11. Trait 与 Impl

### 11.1 定义 trait

```mimi
trait Display {
    func to_string() -> string;
}

trait Comparable {
    func compare_to(other: Self) -> i32;
}
```

### 11.2 实现 trait

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
```

### 11.3 泛型约束

```mimi
func print_item<T>(item: T) where T: Display {
    println(to_string(item));
}

func sort_list<T>(list: List<T>) where T: Comparable {
    // ...
}
```

---

## 12. Extern 块（FFI）

```mimi
extern "C" {
    func read_file(path: string, cap @fh: FileReadCap) -> string;
    func write_file(path: string, data: string, cap @fh: FileWriteCap) -> Result<(), string>;
    func simple_func(x: i32) -> i32;
}
```

- `cap @param: Type` — 移动语义的 cap 参数
- `&param: Type` — 借用语义的 cap 参数
- 无标注 — 普通参数

---

## 13. 模块系统

### 12.1 模块声明

```mimi
module Shop {
    // 模块内容
    func process_order() { ... }
}
```

### 12.2 可见性

```mimi
module Shop {
    pub func process_order() { ... }    // 公开
    func internal_helper() { ... }       // 私有（默认）
}
```

### 12.3 导入

```mimi
use std::collections::Map;
use crate::models::User;
use super::helper;
use another_package::some_func;
```

### 12.4 路径语法

```mimi
let user = User::new("Alice");      // 模块路径用 ::
let name = user.name;               // 字段访问用 .
let display = user.to_string();     // 方法调用用 .
```

---

## 14. 模块与包管理

### 13.1 项目结构

```
my_project/
├── mimi.toml           # 包配置
├── src/
│   ├── main.mimi       # 入口
│   ├── domain.mimi     # 领域模块
│   └── services/
│       └── payment.mimi
└── tests/
    └── integration.mimi
```

### 13.2 mimi.toml

```toml
[package]
name = "shop"
version = "0.1.0"

[dependencies]
std = "1.0"
payment-sdk = { path = "../payment-sdk" }
```

---

## 15. 意图后缀（Commitment Suffix）— 已移除

> 0.1.8 Phase E：MimiSpec 已从 Mimi 拆离，`func$` / `func?` / `func$$` 等
> 意图后缀不再属于语言。`.mms` 文件不再由 `mimi` 读取，`mms{}` 为硬错误。

---

## 16. 属性与 derive

```mimi
#[derive(Debug, Clone, Eq)]
type User {
    name: string
    age: i32
}
```

支持的 derive 宏：
- `Debug` — 自动生成 `to_string()` 方法
- `Clone` — 自动生成 `clone()` 方法
- `Eq` — 自动生成 `eq(other)` 方法

---

## 17. 元数据关键字

### desc / rule / steps（已移除）

> 0.1.8 Phase E：`desc`/`rule`/`steps` 与 `.mms` 草图模式一起移除。
> 它们不再是 Mimi 关键字；`mms{}` 为硬错误。

### math

```mimi
math:
    d_k = dim(key, -1)
    scores = query @ key.T / sqrt(d_k)
```

编译时常量表达式求值块。
