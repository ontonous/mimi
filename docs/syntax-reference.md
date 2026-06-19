# Mimi 语法参考 (v0.3)

> 基于解析器实际实现的语法规范。以代码实证为准。

---

## 1. 文件结构

`.mimi` 文件由顶层条目（item）序列组成，条目之间用空行分隔。

```mimi
// 单行注释

use std::io          // 导入模块

func main() -> i32 {  // 顶层函数
    0
}

type Point {           // 顶层类型定义
    x: f64,
    y: f64,
}
```

---

## 2. 注释

```mimi
// 单行注释

/* 多行
   注释 */
```

---

## 3. 基础类型

| 类型 | 说明 | 示例 |
|------|------|------|
| `i32` | 32 位有符号整数 | `42`, `-7`, `0xFF` |
| `i64` | 64 位有符号整数 | `42L` |
| `f64` | 64 位浮点数 | `3.14`, `-0.5`, `1e10` |
| `bool` | 布尔值 | `true`, `false` |
| `string` | UTF-8 字符串 | `"hello"`, `""` |
| `unit` | 空元组（无返回值） | `()` |
| `nothing` | 不可达类型 | — |

### 3.1 字面量

```mimi
42          // 整数字面量
3.14        // 浮点字面量
true        // 布尔字面量
"hello"     // 字符串字面量
f"val={x}"  // f-string 插值
()          // 单元字面量
```

### 3.2 复合类型

```mimi
List<T>       // 动态数组（泛型）
(T1, T2)      // 元组
[T1, T2, T3]  // 列表字面量
```

---

## 4. 变量与绑定

```mimi
let x = 42               // 不可变绑定
let mut y = 10           // 可变绑定
let s: string = "hello"  // 显式类型标注
let _ = expr             // 忽略绑定
```

### 4.1 赋值

```mimi
y = 20           // 赋值
y += 5           // 复合赋值 (+=, -=, *=, /=)
```

---

## 5. 函数

```mimi
// 基本函数
func add(a: i32, b: i32) -> i32 {
    a + b
}

// pub 可见性
pub func greet(name: string) -> string {
    "Hello, " + name
}

// 无返回值（unit）
func log(msg: string) {
    println(msg)
}

// 泛型函数
pub func identity<T>(x: T) -> T {
    x
}

// 多泛型参数
pub func swap<T, U>(a: T, b: U) -> (U, T) {
    (b, a)
}

// 带 where 约束
func compare<T>(a: T, b: T) -> bool where T: Eq {
    a == b
}

// 契约前缀
func withdraw(amount: i32) -> i32 {
    requires: amount > 0
    ensures: result >= 0
    balance - amount
}

// 可变参数（通过 let mut）
func sum(xs: List<i32>) -> i32 {
    let mut total = 0
    for x in xs {
        total += x
    }
    total
}
```

### 5.1 函数类型

```mimi
func apply(f: func(i32) -> i32, x: i32) -> i32 {
    f(x)
}
```

### 5.2 闭包

```mimi
let double = fn(x: i32) -> i32 { x * 2 }
let adder = fn(a: i32, b: i32) -> i32 { a + b }
```

---

## 6. 控制流

### 6.1 if/else

```mimi
if condition {
    // ...
} else if other {
    // ...
} else {
    // ...
}

// if 表达式
let x = if n > 0 { n } else { -n }
```

### 6.2 while

```mimi
while condition {
    // ...
}

// 带 break/continue
let mut i = 0
while i < 10 {
    if i == 3 { i += 1; continue }
    if i == 7 { break }
    i += 1
}
```

### 6.3 for

```mimi
// 遍历列表
for x in items {
    println(x)
}

// 遍历范围
for i in range(0, 10) {
    println(i)
}

// 带解构
for (name, value) in pairs {
    println(name, "=", value)
}
```

### 6.4 match

```mimi
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
}

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14 * r * r,
        Rectangle(w, h) => w * h,
    }
}

// 带守卫
match x {
    n if n > 0 => "positive",
    n if n < 0 => "negative",
    _ => "zero",
}

// 嵌套解构
match point {
    (0, 0) => "origin",
    (x, 0) => "on x-axis",
    (0, y) => "on y-axis",
    (x, y) => "generic",
}
```

---

## 7. 类型定义

### 7.1 枚举 (ADT)

```mimi
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
    Triangle { a: f64, b: f64, c: f64 }
}

// 构造
let c = Circle(5.0)
let r = Rectangle(3.0, 4.0)
let t = Triangle { a: 3.0, b: 4.0, c: 5.0 }
```

### 7.2 记录 (Record)

```mimi
type Point {
    x: f64,
    y: f64,
}

// 构造
let p = Point { x: 1.0, y: 2.0 }

// 字段访问
let px = p.x
```

### 7.3 类型别名

```mimi
type Meter = f64
type UserId = i64
```

### 7.4 Newtype

```mimi
newtype UserId = i32
newtype OrderId = i32

// 构造
let u = UserId(42)

// 解构
let UserId(v) = u
```

### 7.5 泛型类型

```mimi
type Pair<T, U> {
    first: T,
    second: U,
}
```

### 7.6 属性

```mimi
#[repr(C)]
type CStruct {
    x: i32,
    y: i32,
}

#[repr(transparent)]
newtype Wrapper = i32
```

---

## 8. 泛型

```mimi
// 泛型函数
pub func first<T>(xs: List<T>) -> T {
    xs[0]
}

// 泛型类型
type Container<T> {
    value: T,
}

// 多泛型
func pair<T, U>(a: T, b: U) -> (T, U) {
    (a, b)
}

// Turbofish 显式类型实例化
let x = identity::<i32>(42)
```

---

## 9. 模式匹配

```mimi
// 变量绑定
match x {
    n => n + 1,
}

// 字面量
match x {
    0 => "zero",
    1 => "one",
    _ => "other",
}

// 枚举解构
match opt {
    Some(v) => v,
    None => 0,
}

// 元组解构
match pair {
    (a, b) => a + b,
}

// 守卫
match x {
    n if n > 0 => "positive",
    _ => "non-positive",
}
```

---

## 10. 错误处理

### 10.1 Result 类型

```mimi
type Result<T, E> {
    Ok(T)
    Err(E)
}

func divide(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 {
        Err("division by zero")
    } else {
        Ok(a / b)
    }
}
```

### 10.2 ? 运算符

```mimi
func safe_divide(a: i32, b: i32) -> Result<i32, string> {
    let result = divide(a, b)?;  // Err 时提前返回
    Ok(result * 2)
}
```

### 10.3 Option 类型

```mimi
// T? 是 Option<T> 的语法糖
func find(xs: List<i32>, target: i32) -> (bool, i32) {
    // 返回 (found, index) 元组
}
```

---

## 11. 并发

### 11.1 spawn / await

```mimi
let future = spawn fetch_data(url)
let result = await future
```

### 11.2 parasteps

```mimi
parasteps {
    let a = spawn task_a()
    let b = spawn task_b()
    let r1 = await a
    let r2 = await b
    r1 + r2
}
```

### 11.3 on failure

```mimi
func booking() -> Result<(), string> {
    let seat = reserve_seat()?
    on failure { cancel_seat(seat) }

    let hotel = book_hotel()?
    on failure { cancel_hotel(hotel) }

    Ok(())
}
```

---

## 12. Actor

```mimi
actor Counter {
    mut count: i32 = 0

    func increment() {
        self.count += 1
    }

    func get_count() -> i32 {
        self.count
    }
}

// 使用
let c = Counter.spawn()
await c.increment()
let n = await c.get_count()
```

---

## 13. 契约

```mimi
func withdraw(amount: i32) -> i32 {
    requires: amount > 0
    ensures: result >= 0

    balance - amount
}

// old() 引用函数入口值
func transfer(from: Account, amount: i32) {
    ensures: from.balance == old(from.balance) - amount
    from.balance -= amount
}
```

---

## 14. 线性能力 (cap)

```mimi
cap FileReadCap
cap FileWriteCap
cap FullAccess = FileReadCap + FileWriteCap

func read(path: string, cap: FileReadCap) -> string {
    let data = read_file(path)
    drop(cap)  // 显式消费
    data
}
```

---

## 15. 内存管理

### 15.1 引用

```mimi
let x = 42
let r = &x       // 不可变引用
let rm = &mut x  // 可变引用
```

### 15.2 共享所有权

```mimi
shared x = create_object()
local_shared y = create_object()
```

### 15.3 Arena

```mimi
arena {
    let ref temp = build_graph()
    // temp 生命周期限于 arena 块
}
```

---

## 16. 模块系统

```mimi
// 导入
use std::io
use std::collections::find
use crate::mymodule

// 模块定义
module MyModule {
    pub func helper() -> i32 { 42 }
}

// 路径访问
MyModule::helper()
```

---

## 17. 运算符

### 17.1 算术运算符

| 运算符 | 说明 | 示例 |
|--------|------|------|
| `+` | 加法 / 字符串拼接 | `a + b` |
| `-` | 减法 / 取负 | `a - b`, `-x` |
| `*` | 乘法 | `a * b` |
| `/` | 除法 | `a / b` |
| `%` | 取模 | `a % b` |
| `**` | 幂运算 | `a ** b` |

### 17.2 比较运算符

| 运算符 | 说明 |
|--------|------|
| `==` | 等于 |
| `!=` | 不等于 |
| `<` | 小于 |
| `>` | 大于 |
| `<=` | 小于等于 |
| `>=` | 大于等于 |

### 17.3 逻辑运算符

| 运算符 | 说明 |
|--------|------|
| `&&` | 逻辑与 |
| `\|\|` | 逻辑或 |
| `!` | 逻辑非 |

### 17.4 位运算符

| 运算符 | 说明 |
|--------|------|
| `&` | 按位与 |
| `\|` | 按位或 |
| `^` | 按位异或 |
| `<<` | 左移 |
| `>>` | 右移 |

### 17.5 其他运算符

| 运算符 | 说明 | 示例 |
|--------|------|------|
| `.` | 字段访问 | `p.x` |
| `[]` | 索引 | `xs[0]` |
| `..` | 范围 | `0..10` |
| `..=` | 闭区间范围 | `0..=10` |
| `?` | 错误传播 | `result?` |
| `@` | 捕获绑定 | `x @ Pattern` |

---

## 18. 内置函数

### 18.1 I/O

| 函数 | 签名 | 说明 |
|------|------|------|
| `println` | `(args...)` | 打印到 stdout + 换行 |
| `eprintln` | `(args...)` | 打印到 stderr + 换行 |
| `input` | `() -> Result<string, string>` | 读取 stdin 一行 |

### 18.2 列表操作

| 函数 | 签名 | 说明 |
|------|------|------|
| `len` | `(xs) -> i32` | 获取长度 |
| `push` | `(xs, elem)` | 追加元素 |
| `pop` | `(xs) -> T` | 弹出末尾元素 |
| `contains` | `(xs, elem) -> bool` | 是否包含 |
| `range` | `(start, end) -> List<i32>` | 生成整数范围 |
| `reverse` | `(xs) -> List<T>` | 反转列表 |
| `sort` | `(xs) -> List<T>` | 排序 |
| `filter` | `(xs, f) -> List<T>` | 过滤 |
| `map` | `(xs, f) -> List<U>` | 映射 |
| `reduce` | `(xs, f, init) -> T` | 归约 |
| `zip` | `(xs, ys) -> List<(T, U)>` | 配对 |
| `enumerate` | `(xs) -> List<(i32, T)>` | 带索引遍历 |
| `sum` | `(xs) -> i32` | 求和 |
| `flatten` | `(xss) -> List<T>` | 扁平化 |

### 18.3 数学

| 函数 | 签名 | 说明 |
|------|------|------|
| `abs` | `(x) -> T` | 绝对值 |
| `min` | `(a, b) -> T` | 最小值 |
| `max` | `(a, b) -> T` | 最大值 |
| `sqrt` | `(x: f64) -> f64` | 平方根 |
| `pow` | `(base, exp) -> f64` | 幂运算 |
| `floor` | `(x: f64) -> i64` | 向下取整 |
| `ceil` | `(x: f64) -> i64` | 向上取整 |
| `round` | `(x: f64) -> i64` | 四舍五入 |
| `random` | `() -> f64` | 随机数 [0, 1) |
| `pi` | `() -> f64` | 圆周率 |

### 18.4 类型转换

| 函数 | 签名 | 说明 |
|------|------|------|
| `to_string` | `(x) -> string` | 转为字符串 |
| `to_int` | `(x) -> i32` | 转为整数 |
| `to_float` | `(x) -> f64` | 转为浮点数 |

### 18.5 字符串操作

| 函数 | 签名 | 说明 |
|------|------|------|
| `str_split` | `(s, delim) -> List<string>` | 分割 |
| `str_join` | `(parts, sep) -> string` | 连接 |
| `str_replace` | `(s, from, to) -> string` | 替换 |
| `str_char_at` | `(s, i) -> string` | 取字符 |
| `str_substring` | `(s, start, end) -> string` | 子串 |
| `str_starts_with` | `(s, prefix) -> bool` | 前缀匹配 |
| `str_ends_with` | `(s, suffix) -> bool` | 后缀匹配 |
| `str_to_upper` | `(s) -> string` | 转大写 |
| `str_to_lower` | `(s) -> string` | 转小写 |
| `str_trim` | `(s) -> string` | 去空白 |
| `str_repeat` | `(s, n) -> string` | 重复 |
| `str_index_of` | `(s, sub) -> (bool, i32)` | 查找子串 |
| `str_parse_int` | `(s) -> (bool, i32)` | 解析整数 |
| `str_parse_float` | `(s) -> (bool, f64)` | 解析浮点数 |

### 18.6 文件系统

| 函数 | 签名 | 说明 |
|------|------|------|
| `file_exists` | `(path) -> bool` | 文件是否存在 |
| `read_file` | `(path) -> string` | 读取文件 |
| `write_file` | `(path, content)` | 写入文件 |

### 18.7 网络

| 函数 | 签名 | 说明 |
|------|------|------|
| `socket` | `(domain, type, proto) -> i32` | 创建 socket |
| `connect` | `(fd, host, port) -> i32` | 连接 |
| `bind` | `(fd, port) -> i32` | 绑定 |
| `listen` | `(fd, backlog) -> i32` | 监听 |
| `send` | `(fd, data) -> i32` | 发送 |
| `recv` | `(fd, size) -> string` | 接收 |
| `close_fd` | `(fd)` | 关闭 |
| `http_get` | `(url) -> string` | HTTP GET |
| `http_post` | `(url, body) -> string` | HTTP POST |

### 18.8 Map 操作

| 函数 | 签名 | 说明 |
|------|------|------|
| `map_new` | `() -> Record` | 创建空 map |
| `map_get` | `(m, key) -> (bool, Any)` | 获取值 |
| `map_set` | `(m, key, value) -> Record` | 设置值 |
| `map_has_key` | `(m, key) -> bool` | 是否有键 |
| `map_remove` | `(m, key) -> Record` | 删除键 |
| `map_size` | `(m) -> i32` | 大小 |
| `map_from_list` | `(pairs) -> Record` | 从列表创建 |

### 18.9 JSON

| 函数 | 签名 | 说明 |
|------|------|------|
| `json_get_string` | `(json, key) -> string` | 获取字符串值 |
| `json_get_int` | `(json, key) -> i64` | 获取整数值 |
| `json_get_element` | `(json, index) -> string` | 获取数组元素 |

### 18.10 时间

| 函数 | 签名 | 说明 |
|------|------|------|
| `now` | `() -> i64` | 当前时间戳（秒） |
| `now_ms` | `() -> i64` | 当前时间戳（毫秒） |
| `sleep` | `(ms)` | 休眠（毫秒） |

### 18.11 环境

| 函数 | 签名 | 说明 |
|------|------|------|
| `getenv` | `(name) -> string` | 获取环境变量 |
| `args` | `() -> List<string>` | 获取命令行参数 |

### 18.12 断言

| 函数 | 签名 | 说明 |
|------|------|------|
| `assert` | `(cond)` | 断言为真 |
| `assert_eq` | `(a, b)` | 断言相等 |
| `assert_ne` | `(a, b)` | 断言不等 |
| `assert_approx_eq` | `(a, b)` | 断言近似相等 |

---

## 19. 标准库模块

| 模块 | 说明 |
|------|------|
| `std::io` | I/O 操作：`print_line`, `print_err`, `print_lines`, `print_bool`, `print_int`, `print_float`, `print_list`, `input_line`, `input_int`, `print_raw`, `print_format`, `input_float`, `input_bool` |
| `std::fs` | 文件系统：`exists`, `read`, `write`, `read_lines`, `write_lines`, `file_size` |
| `std::strings` | 字符串操作：`is_empty`, `char_at`, `substring`, `to_upper`, `to_lower`, `trim`, `split`, `join`, `contains`, `capitalize`, `title`, `reverse_string`, `truncate`, `pad_left`, `pad_right`, `lines`, `words`, `count_char`, `indent`, `quote`, `count_substring`, `is_blank`, `replace_all` |
| `std::collections` | 集合操作：`find`, `dedup`, `concat`, `take`, `drop_n`, `sort_list`, `sum`, `map_list`, `unique`, `any`, `all`, `partition`, `group_by`, `chunks`, `intersperse`, `min_list`, `max_list`, `remove_at`, `fill_list`, `range_step`, `filter_list`, `reduce_list` |
| `std::random` | 随机数工具：`random_float`, `random_int`, `random_choice`, `random_bool`, `random_sample`, `shuffle` |
| `std::text` | 文本处理：`is_blank`, `is_numeric`, `count_lines`, `slugify`, `indent_text`, `wrap_text`, `camel_to_snake` |
| `std::result` | Result 组合子：`is_ok_result`, `is_err_result`, `result_unwrap`, `unwrap_or`, `expect_result`, `map_result`, `map_err_result` |
| `std::mymath` | 数学函数：`square`, `cube`, `abs`, `abs_float`, `factorial`, `fibonacci`, `is_prime`, `gcd`, `lcm`, `hypot`, `power`, `sqrt_val`, `floor_val`, `ceil_val`, `round_val`, `clamp_int`, `collatz_steps`, `mod_pow`, `deg_to_rad`, `rad_to_deg`, `random_int`, `is_power_of_two`, `next_power_of_two` |
| `std::net` | 网络操作（返回 `Result<T, NetError>`）：`tcp_socket`, `tcp_connect`, `tcp_listen`, `tcp_send`, `tcp_recv`, `fetch`, `fetch_post`；错误类型：`NetError { SocketCreate, ConnectFailed, BindFailed, ListenFailed, AcceptFailed, SendFailed, RecvFailed, HttpGetFailed, HttpPostFailed }` |
| `std::maps` | Map 操作：`new`, `get`, `set`, `has_key`, `remove`, `size`, `is_empty`, `get_or_default`, `merge`, `to_list`, `filter_keys`, `map_values`, `update`, `pick`, `omit` |
| `std::json` | JSON 操作：`to_json`, `from_json`, `get_string`, `get_int`, `get_element`, `get_bool`, `get_float`, `is_valid_json`, `array_length` |
| `std::time` | 时间操作：`timestamp`, `timestamp_ms`, `sleep_ms`, `elapsed`, `seconds_since`, `millis_since`, `duration` |
| `std::datetime` | 日期时间：`format_duration_secs`, `format_duration_ms`, `days_from_now`, `hours_from_now`, `is_future`, `is_past`, `time_since`, `time_until`, `sleep_until` |
| `std::env` | 环境操作：`get_var`, `cli_args`, `get_var_or`, `has_var`, `get_int`, `get_float`, `arg_count`, `first_arg` |
| `std::testing` | 测试工具：`assert_eq_int`, `assert_ne_int`, `assert_approx_eq_float`, `assert_true`, `assert_false`, `assert_eq_string`, `assert_eq_bool` |
| `std::prelude` | 基础工具：`identity`, `const_val`, `is_even`, `is_odd`, `min3`, `max3`, `swap`, `clamp`, `lerp`, `compose`, `pipe`, `tap`, `fail`, `todo`, `unreachable`, `assert_msg`, `repeat_action`, `times`, `type_of`, `to_int_safe`, `to_float_safe` |

> **Codegen 兼容性说明**：`random_int`、`is_power_of_two`、`next_power_of_two` 现已支持 codegen 编译。`to_int` / `to_float` 内置函数在 codegen 中可直接接受数值类型（不仅限于字符串）。

---

## 20. 编译与运行

```bash
# 类型检查
mimi check file.mimi

# 运行
mimi run file.mimi

# 编译
mimi build file.mimi

# 编译并验证合约 (requires/ensures 编译为运行时 assert)
mimi build file.mimi --verify-contracts

# 格式化
mimi fmt file.mimi
mimi fmt --check file.mimi

# 静态分析
mimi lint file.mimi

# LSP 服务器
mimi lsp

# 包管理
mimi init                # 初始化新项目
mimi add <name>          # 添加依赖
mimi install             # 安装依赖
mimi tree                # 显示依赖树
mimi list                # 列出依赖
mimi publish             # 发布到本地 registry

# 运行测试
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test
```

---

## 21. 项目配置 (mimi.toml)

```toml
[package]
name = "my-project"
version = "0.1.0"
description = "A Mimi project"
entry = "main.mimi"

[[dependencies]]
name = "my-lib"
version = "^1.0"

[[dependencies]]
name = "helper"
path = "../helper"

[[dependencies]]
name = "utils"
git = "https://github.com/user/utils"
tag = "v2.0"

[registry]
url = "https://registry.mimi-lang.org"
```

### 依赖来源

| 字段 | 说明 |
|------|------|
| `version` | 版本约束 (semver: `^1.0`, `>=0.5, <2.0`, `*`) |
| `path` | 本地路径依赖 |
| `git` | Git 仓库 URL |
| `tag` | Git 分支/标签 (默认 `main`) |

---

---

## 22. 语法速查

### 函数声明

```mimi
pub func name<T>(param: Type) -> RetType { body }
```

### 类型定义

```mimi
type Name { Variant(Type) | Record { field: Type } }
type Alias = Type
newtype Name = Type
```

### 变量

```mimi
let x = expr
let mut x = expr
let x: Type = expr
```

### 控制流

```mimi
if cond { ... } else { ... }
while cond { ... }
for x in iter { ... }
match expr { Pattern => expr, ... }
```

### 错误处理

```mimi
func f() -> Result<T, E> {
    let v = expr?;  // ? 传播错误
    Ok(v)
}
```

### 泛型

```mimi
func f<T>(x: T) -> T { x }
type Container<T> { value: T }
```

### 模块

```mimi
use path::to::module
module Name { ... }
```

---

*本规范基于 Mimi v0.3.1 / v1.0.0-rc.1 解析器实现。*
